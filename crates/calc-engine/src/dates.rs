//! Excel date serials (the 1900 system, with its phantom 29 February 1900) and parsing typed dates and times.

/// Days from 0000-03-01 to the civil date (proleptic Gregorian), Howard Hinnant's algorithm.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Unix days of 1899-12-30, Excel's day 0 for every serial after 28 February 1900.
const EPOCH_1899_12_30: i64 = -25569;

/// The serial of a date. Months and days may overflow (DATE(2026,14,1) is February 2027), as in Excel.
pub fn serial_from_ymd(y: i64, m: i64, d: i64) -> Option<f64> {
    // Excel treats years 0–1899 as 1900–3799.
    let y = if (0..1900).contains(&y) { y + 1900 } else { y };
    if !(1900..=9999).contains(&y) {
        return None;
    }
    let total_months = y * 12 + (m - 1);
    let (yy, mm) = (total_months.div_euclid(12), total_months.rem_euclid(12) + 1);
    let base = days_from_civil(yy, mm as u32, 1) + (d - 1);
    let serial = base - EPOCH_1899_12_30;
    // Serials 1..=59 are 1900-01-01..1900-02-28 and sit one lower than the 1899-12-30 epoch gives,
    // because Excel counts a 29 February 1900 that never happened.
    let serial = if serial <= 60 { serial - 1 } else { serial };
    if serial < 0 {
        return None;
    }
    Some(serial as f64)
}

/// The civil date of a serial's day part. Serial 60 is Excel's 1900-02-29.
pub fn ymd_from_serial(serial: f64) -> Option<(i64, u32, u32)> {
    if !serial.is_finite() || serial < 0.0 || serial >= 2958466.0 {
        return None;
    }
    let day = serial.floor() as i64;
    if day == 0 {
        return Some((1900, 1, 0));
    }
    if day == 60 {
        return Some((1900, 2, 29));
    }
    let adjusted = if day < 60 { day + 1 } else { day };
    Some(civil_from_days(adjusted + EPOCH_1899_12_30))
}

/// Hours, minutes, seconds and milliseconds of a serial's fraction (rounded to the millisecond).
pub fn hms_from_serial(serial: f64) -> (u32, u32, u32, u32) {
    let frac = serial - serial.floor();
    let ms = (frac * 86_400_000.0).round() as i64;
    let ms = ms.clamp(0, 86_399_999);
    let h = ms / 3_600_000;
    let m = (ms % 3_600_000) / 60_000;
    let s = (ms % 60_000) / 1000;
    (h as u32, m as u32, s as u32, (ms % 1000) as u32)
}

/// Day of the week, 0 = Sunday.
pub fn weekday_from_serial(serial: f64) -> u32 {
    let day = serial.floor() as i64;
    // Serial 1 (1900-01-01) is treated as a Sunday by Excel (because of the phantom leap day, true dates before
    // 1 March 1900 are off by one, and Excel keeps that).
    (day.rem_euclid(7) as u32 + 6) % 7
}

pub fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
    }
}

/// The serial for a moment given as Unix seconds (UTC).
pub fn serial_from_unix(secs: f64) -> f64 {
    secs / 86400.0 - EPOCH_1899_12_30 as f64
}

pub const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November",
    "December",
];
pub const WEEKDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

/// What a typed date or time looks like, so the cell gets a matching number format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateShape {
    /// 2026-10-17
    IsoDate,
    /// 10/17/2026
    UsDate,
    /// 17 Oct 2026, Oct 17, 2026
    NamedDate,
    Time,
    TimeSeconds,
    IsoDateTime,
}

impl DateShape {
    pub fn format_code(self) -> &'static str {
        match self {
            Self::IsoDate => "yyyy-mm-dd",
            Self::UsDate => "m/d/yyyy",
            Self::NamedDate => "d mmm yyyy",
            Self::Time => "h:mm",
            Self::TimeSeconds => "h:mm:ss",
            Self::IsoDateTime => "yyyy-mm-dd h:mm:ss",
        }
    }
}

fn month_from_name(s: &str) -> Option<u32> {
    let l = s.to_ascii_lowercase();
    if l.len() < 3 {
        return None;
    }
    MONTHS
        .iter()
        .position(|m| m.to_ascii_lowercase().starts_with(&l) || (l.len() == 4 && l == "sept" && m.starts_with("Sep")))
        .map(|i| i as u32 + 1)
}

fn parse_time(t: &str) -> Option<(f64, bool)> {
    let t = t.trim();
    let (body, pm) = {
        let l = t.to_ascii_lowercase();
        if let Some(b) = l.strip_suffix("pm") {
            (b.trim().to_owned(), Some(true))
        } else if let Some(b) = l.strip_suffix("am") {
            (b.trim().to_owned(), Some(false))
        } else {
            (l, None)
        }
    };
    let parts: Vec<&str> = body.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let h: u32 = parts[0].parse().ok()?;
    let m: u32 = parts[1].parse().ok()?;
    let s: f64 = if parts.len() == 3 { parts[2].parse().ok()? } else { 0.0 };
    if m >= 60 || s >= 60.0 {
        return None;
    }
    let h = match pm {
        Some(true) if h < 12 => h + 12,
        Some(false) if h == 12 => 0,
        Some(_) if h > 12 => return None,
        _ => h,
    };
    if h >= 24 && pm.is_some() {
        return None;
    }
    Some(((h as f64 * 3600.0 + m as f64 * 60.0 + s) / 86400.0, parts.len() == 3))
}

fn parse_date(t: &str) -> Option<(f64, DateShape)> {
    let t = t.trim();
    // 2026-10-17 or 2026/10/17
    let iso: Vec<&str> = t.split(['-', '/']).collect();
    if iso.len() == 3 && iso[0].len() == 4 && iso.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    {
        let (y, m, d) = (iso[0].parse().ok()?, iso[1].parse::<i64>().ok()?, iso[2].parse::<i64>().ok()?);
        if (1..=12).contains(&m) && d >= 1 && d <= days_in_month(y, m as u32) as i64 {
            return Some((serial_from_ymd(y, m, d)?, DateShape::IsoDate));
        }
        return None;
    }
    // 10/17/2026 (month first, as Excel in en-US)
    let us: Vec<&str> = t.split('/').collect();
    if us.len() == 3 && us.iter().all(|p| !p.is_empty() && p.len() <= 4 && p.bytes().all(|b| b.is_ascii_digit())) {
        let (m, d, y) = (us[0].parse::<i64>().ok()?, us[1].parse::<i64>().ok()?, us[2].parse::<i64>().ok()?);
        let y = if us[2].len() <= 2 { if y < 30 { 2000 + y } else { 1900 + y } } else { y };
        if (1..=12).contains(&m) && d >= 1 && d <= days_in_month(y, m as u32) as i64 {
            return Some((serial_from_ymd(y, m, d)?, DateShape::UsDate));
        }
        return None;
    }
    // 17 Oct 2026, 17-Oct-2026, Oct 17 2026, Oct 17, 2026, October 17, 2026
    let words: Vec<&str> = t.split(|c: char| c == ' ' || c == '-' || c == ',').filter(|w| !w.is_empty()).collect();
    if words.len() == 3 {
        let try_order = |d: &str, m: &str, y: &str| -> Option<f64> {
            let d: i64 = d.parse().ok()?;
            let m = month_from_name(m)? as i64;
            if y.len() != 4 {
                return None;
            }
            let y: i64 = y.parse().ok()?;
            if d < 1 || d > days_in_month(y, m as u32) as i64 {
                return None;
            }
            serial_from_ymd(y, m, d)
        };
        if let Some(s) = try_order(words[0], words[1], words[2]).or_else(|| try_order(words[1], words[0], words[2])) {
            return Some((s, DateShape::NamedDate));
        }
    }
    None
}

/// A typed date, time or both: "2026-10-17", "10/17/2026", "17 Oct 2026", "14:30", "2:30 PM",
/// "2026-10-17 14:30:00", "2026-10-17T14:30:00Z".
pub fn parse_date_time(t: &str) -> Option<(f64, DateShape)> {
    let t = t.trim();
    if t.is_empty() || t.len() > 40 {
        return None;
    }
    if let Some(d) = parse_date(t) {
        return Some(d);
    }
    if t.contains(':')
        && let Some((time, _secs)) = parse_time(t)
    {
        let shape = if t.matches(':').count() == 2 { DateShape::TimeSeconds } else { DateShape::Time };
        return Some((time, shape));
    }
    // Date and time.
    let t2 = t.trim_end_matches('Z');
    let split = t2.find('T').filter(|&i| i == 10).or_else(|| t2.find(' '));
    if let Some(i) = split {
        let (d, rest) = t2.split_at(i);
        let rest = &rest[1..];
        if let (Some((ds, _)), Some((ts, _))) = (parse_date(d), parse_time(rest.split('.').next().unwrap_or(rest))) {
            return Some((ds + ts, DateShape::IsoDateTime));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serials_match_excel() {
        assert_eq!(serial_from_ymd(1900, 1, 1), Some(1.0));
        assert_eq!(serial_from_ymd(1900, 2, 28), Some(59.0));
        assert_eq!(serial_from_ymd(1900, 3, 1), Some(61.0));
        assert_eq!(serial_from_ymd(2026, 10, 17), Some(46312.0));
        assert_eq!(ymd_from_serial(46312.0), Some((2026, 10, 17)));
        assert_eq!(ymd_from_serial(60.0), Some((1900, 2, 29)));
        assert_eq!(ymd_from_serial(61.0), Some((1900, 3, 1)));
        assert_eq!(serial_from_ymd(2026, 13, 1), serial_from_ymd(2027, 1, 1));
        assert_eq!(serial_from_ymd(2026, 3, 0), serial_from_ymd(2026, 2, 28));
    }

    #[test]
    fn weekday() {
        // 2026-10-17 is a Saturday.
        assert_eq!(weekday_from_serial(46312.0), 6);
    }

    #[test]
    fn parses() {
        assert_eq!(parse_date_time("2026-10-17").map(|d| d.0), Some(46312.0));
        assert_eq!(parse_date_time("10/17/2026").map(|d| d.0), Some(46312.0));
        assert_eq!(parse_date_time("17 Oct 2026").map(|d| d.0), Some(46312.0));
        assert_eq!(parse_date_time("Oct 17, 2026").map(|d| d.0), Some(46312.0));
        assert_eq!(parse_date_time("12:00").map(|d| d.0), Some(0.5));
        assert_eq!(parse_date_time("2:30 PM").map(|d| d.1), Some(DateShape::Time));
        assert!(parse_date_time("2026-10-17T12:00:00Z").is_some());
        assert!(parse_date_time("hello").is_none());
        assert!(parse_date_time("2026-02-30").is_none());
    }
}
