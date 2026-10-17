//! Excel's built-in number formats (the ids a file can use without defining them), as an en-US Excel shows them.

/// The code for a built-in number format id.
pub(crate) fn builtin(id: u32) -> Option<&'static str> {
    Some(match id {
        0 => "General",
        1 => "0",
        2 => "0.00",
        3 => "#,##0",
        4 => "#,##0.00",
        5 => "\"$\"#,##0_);\\(\"$\"#,##0\\)",
        6 => "\"$\"#,##0_);[Red]\\(\"$\"#,##0\\)",
        7 => "\"$\"#,##0.00_);\\(\"$\"#,##0.00\\)",
        8 => "\"$\"#,##0.00_);[Red]\\(\"$\"#,##0.00\\)",
        9 => "0%",
        10 => "0.00%",
        11 => "0.00E+00",
        12 => "# ?/?",
        13 => "# ??/??",
        // Excel writes the short date as 14 and shows it in the locale's form: m/d/yyyy in en-US.
        14 => "m/d/yyyy",
        15 => "d-mmm-yy",
        16 => "d-mmm",
        17 => "mmm-yy",
        18 => "h:mm AM/PM",
        19 => "h:mm:ss AM/PM",
        20 => "h:mm",
        21 => "h:mm:ss",
        22 => "m/d/yyyy h:mm",
        // East Asian date and time formats: shown as dates and times here.
        27..=31 | 34..=36 | 50..=58 => "yyyy-mm-dd",
        32 => "h:mm",
        33 => "h:mm:ss",
        37 => "#,##0 ;(#,##0)",
        38 => "#,##0 ;[Red](#,##0)",
        39 => "#,##0.00;(#,##0.00)",
        40 => "#,##0.00;[Red](#,##0.00)",
        41 => "_(* #,##0_);_(* \\(#,##0\\);_(* \"-\"_);_(@_)",
        42 => "_(\"$\"* #,##0_);_(\"$\"* \\(#,##0\\);_(\"$\"* \"-\"_);_(@_)",
        43 => "_(* #,##0.00_);_(* \\(#,##0.00\\);_(* \"-\"??_);_(@_)",
        44 => "_(\"$\"* #,##0.00_);_(\"$\"* \\(#,##0.00\\);_(\"$\"* \"-\"??_);_(@_)",
        45 => "mm:ss",
        46 => "[h]:mm:ss",
        47 => "mm:ss.0",
        48 => "##0.0E+0",
        49 => "@",
        _ => return None,
    })
}

/// What a date or time format shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DateKind {
    Date,
    Time,
    DateTime,
    /// Elapsed time: `[h]:mm:ss`.
    Duration,
}

/// Whether a format code shows a date, a time, both, or elapsed time (None for every other format). Only the first
/// section counts; quoted text, escaped characters and colours are skipped.
pub(crate) fn date_kind(code: &str) -> Option<DateKind> {
    let (mut date, mut time, mut elapsed, mut month_or_minute) = (false, false, false, false);
    let mut chars = code.chars().peekable();
    let mut in_quote = false;
    while let Some(c) = chars.next() {
        if in_quote {
            if c == '"' {
                in_quote = false;
            }
            continue;
        }
        match c {
            ';' => break,
            '"' => in_quote = true,
            '\\' | '_' | '*' => {
                chars.next();
            }
            '[' => {
                let mut inner = String::new();
                for n in chars.by_ref() {
                    if n == ']' {
                        break;
                    }
                    inner.push(n);
                }
                let l = inner.to_ascii_lowercase();
                if !l.is_empty() && l.chars().all(|x| matches!(x, 'h' | 'm' | 's')) {
                    elapsed = true;
                    time = true;
                }
            }
            'y' | 'Y' | 'd' | 'D' => date = true,
            'h' | 'H' | 's' | 'S' => time = true,
            'm' | 'M' => month_or_minute = true,
            'a' | 'A' if chars.peek().is_some_and(|n| *n == 'm' || *n == 'M' || *n == '/') => {
                time = true;
                chars.next();
            }
            _ => {}
        }
    }
    if month_or_minute && !time {
        date = true;
    }
    match (date, time) {
        _ if elapsed && !date => Some(DateKind::Duration),
        (true, true) => Some(DateKind::DateTime),
        (true, false) => Some(DateKind::Date),
        (false, true) => Some(DateKind::Time),
        (false, false) => None,
    }
}

/// A number format code as the engine keeps it: General is no format.
pub(crate) fn engine_code(code: &str) -> Option<String> {
    let c = code.trim();
    if c.is_empty() || c.eq_ignore_ascii_case("general") { None } else { Some(c.to_owned()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins() {
        assert_eq!(builtin(14), Some("m/d/yyyy"));
        assert_eq!(builtin(9), Some("0%"));
        assert_eq!(builtin(163), None);
        assert!(crate::numfmt::is_date_format(builtin(22).unwrap_or_default()));
        assert_eq!(engine_code("General"), None);
        assert_eq!(engine_code("0.00"), Some("0.00".into()));
    }

    #[test]
    fn date_kinds() {
        assert_eq!(date_kind("yyyy-mm-dd"), Some(DateKind::Date));
        assert_eq!(date_kind("mmm-yy"), Some(DateKind::Date));
        assert_eq!(date_kind("mmmm"), Some(DateKind::Date));
        assert_eq!(date_kind("h:mm AM/PM"), Some(DateKind::Time));
        assert_eq!(date_kind("mm:ss"), Some(DateKind::Time));
        assert_eq!(date_kind("m/d/yyyy h:mm"), Some(DateKind::DateTime));
        assert_eq!(date_kind("[h]:mm:ss"), Some(DateKind::Duration));
        assert_eq!(date_kind("[$-409]d-mmm-yy;@"), Some(DateKind::Date));
        assert_eq!(date_kind("0.00"), None);
        assert_eq!(date_kind("#,##0 \"days\""), None);
        assert_eq!(date_kind("[Red]0%"), None);
        assert_eq!(date_kind("General"), None);
        for id in [14, 15, 16, 17, 18, 19, 20, 21, 22, 45, 46, 47] {
            assert!(date_kind(builtin(id).unwrap_or_default()).is_some(), "{id}");
        }
    }
}
