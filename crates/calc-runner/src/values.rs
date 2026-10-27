//! Values crossing between the engine, Arrow (DuckDB results, Parquet) and JSON (JavaScript, answers).
//!
//! Dates are Excel serials in the engine (the 1900 system, phantom 29 February 1900 included) and days or
//! microseconds since 1970-01-01 everywhere else; the helpers here convert between the two.

use std::sync::Arc;

use calc_engine::{ErrorKind, Value, numfmt};
use duckdb::arrow::{
    array::{Array, ArrayRef, AsArray},
    compute::cast,
    datatypes::{
        DataType, Date32Type, Date64Type, Float64Type, Schema, Time32MillisecondType,
        Time32SecondType, Time64MicrosecondType, Time64NanosecondType, TimeUnit,
        TimestampMicrosecondType, TimestampMillisecondType, TimestampNanosecondType,
        TimestampSecondType,
    },
    record_batch::RecordBatch,
    util::display::{ArrayFormatter, FormatOptions},
};

use crate::{OutColumn, RunError};

/// The format a date column gets when written into cells.
pub const DATE_FORMAT: &str = "yyyy-mm-dd";
/// The format a date-and-time column gets.
pub const DATETIME_FORMAT: &str = "yyyy-mm-dd hh:mm:ss";
/// The format a time-of-day column gets.
pub const TIME_FORMAT: &str = "hh:mm:ss";

const MICROS_PER_DAY: i64 = 86_400_000_000;

/// The serial of a day given as days since 1970-01-01.
pub fn serial_from_unix_days(days: i64) -> f64 {
    let s = days + 25_569;
    (if s <= 60 { s - 1 } else { s }) as f64
}

/// Days since 1970-01-01 of a serial's day (serial 60, Excel's 29 February 1900, becomes 1 March).
pub fn unix_days_from_serial(serial: f64) -> i64 {
    let d = serial.floor() as i64;
    if d >= 61 { d - 25_569 } else { d - 25_568 }
}

pub fn serial_from_unix_micros(us: i64) -> f64 {
    let days = us.div_euclid(MICROS_PER_DAY);
    let rem = us.rem_euclid(MICROS_PER_DAY);
    serial_from_unix_days(days) + rem as f64 / MICROS_PER_DAY as f64
}

pub fn unix_micros_from_serial(serial: f64) -> i64 {
    let frac = serial - serial.floor();
    unix_days_from_serial(serial) * MICROS_PER_DAY + (frac * MICROS_PER_DAY as f64).round() as i64
}

/// Microseconds since midnight of a serial's time part.
pub fn day_micros_from_serial(serial: f64) -> i64 {
    let frac = serial - serial.floor();
    ((frac * MICROS_PER_DAY as f64).round() as i64).clamp(0, MICROS_PER_DAY - 1)
}

/// `(year, month, day)` of days since 1970-01-01 (proleptic Gregorian).
pub fn civil_from_unix_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Days since 1970-01-01 of a civil date.
pub fn unix_days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `2026-10-17` for a serial.
pub fn iso_date(serial: f64) -> String {
    let (y, m, d) = civil_from_unix_days(unix_days_from_serial(serial));
    format!("{y:04}-{m:02}-{d:02}")
}

/// `13:45:00` (with `.250` when there are milliseconds) for a serial's time part.
pub fn iso_time(serial: f64) -> String {
    let us = day_micros_from_serial(serial);
    let (h, rest) = (us / 3_600_000_000, us % 3_600_000_000);
    let (mi, rest) = (rest / 60_000_000, rest % 60_000_000);
    let (s, us) = (rest / 1_000_000, rest % 1_000_000);
    let ms = (us + 500) / 1000;
    if ms > 0 && ms < 1000 {
        format!("{h:02}:{mi:02}:{s:02}.{ms:03}")
    } else {
        format!("{h:02}:{mi:02}:{s:02}")
    }
}

/// `2026-10-17T13:45:00` for a serial.
pub fn iso_datetime(serial: f64) -> String {
    let us = unix_micros_from_serial(serial);
    let days = us.div_euclid(MICROS_PER_DAY);
    let (y, m, d) = civil_from_unix_days(days);
    let in_day = us.rem_euclid(MICROS_PER_DAY) as f64 / MICROS_PER_DAY as f64;
    format!("{y:04}-{m:02}-{d:02}T{}", iso_time(in_day))
}

/// The serial of an ISO date or date-time (`2026-10-17`, `2026-10-17T13:45:00`, `2026-10-17 13:45:00.5`, with an
/// optional `Z` or offset, which is applied so the result is UTC).
pub fn serial_from_iso(text: &str) -> Option<f64> {
    let t = text.trim();
    let (date, time) = match t.find(['T', ' ']) {
        Some(i) => (&t[..i], Some(&t[i + 1..])),
        None => (t, None),
    };
    let mut parts = date.splitn(3, '-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let mut micros = unix_days_from_civil(y, m, d) * MICROS_PER_DAY;
    if let Some(time) = time {
        let (clock, offset_minutes) = split_offset(time)?;
        let mut hms = clock.splitn(3, ':');
        let h: i64 = hms.next()?.parse().ok()?;
        let mi: i64 = hms.next().unwrap_or("0").parse().ok()?;
        let s: f64 = hms.next().unwrap_or("0").parse().ok()?;
        micros += h * 3_600_000_000 + mi * 60_000_000 + (s * 1_000_000.0).round() as i64;
        micros -= offset_minutes * 60_000_000;
    }
    Some(serial_from_unix_micros(micros))
}

fn split_offset(time: &str) -> Option<(&str, i64)> {
    if let Some(c) = time.strip_suffix('Z') {
        return Some((c, 0));
    }
    if let Some(i) = time.rfind(['+', '-']) {
        let (clock, off) = time.split_at(i);
        let sign = if off.starts_with('-') { -1 } else { 1 };
        let off = &off[1..];
        let (h, m) = off
            .split_once(':')
            .unwrap_or((off.get(..2).unwrap_or(off), off.get(2..).unwrap_or("0")));
        let h: i64 = h.parse().ok()?;
        let m: i64 = if m.is_empty() { 0 } else { m.parse().ok()? };
        return Some((clock, sign * (h * 60 + m)));
    }
    Some((time, 0))
}

/// What a result column holds, as far as its cells' format goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Plain,
    Date,
    DateTime,
    Time,
}

/// Gathers a result, batch by batch, into columns of engine values: keeps the first `keep` rows (all when
/// None), counts every row, and refuses results bigger than `max_cells`.
pub(crate) struct Collector {
    names: Vec<String>,
    kinds: Vec<Kind>,
    has_time: Vec<bool>,
    data: Vec<Vec<Value>>,
    keep: Option<usize>,
    count: u64,
    max_cells: u64,
}

impl Collector {
    pub fn new(schema: &Schema, keep: Option<usize>, max_cells: u64) -> Self {
        let names: Vec<String> = schema.fields().iter().map(|f| f.name().clone()).collect();
        let kinds: Vec<Kind> = schema
            .fields()
            .iter()
            .map(|f| kind_of(f.data_type()))
            .collect();
        let n = names.len();
        Self {
            names,
            kinds,
            has_time: vec![false; n],
            data: vec![Vec::new(); n],
            keep,
            count: 0,
            max_cells,
        }
    }

    pub fn push_batch(&mut self, batch: &RecordBatch) -> Result<(), RunError> {
        let rows = batch.num_rows();
        let start = self.count;
        self.count += rows as u64;
        let cols = self.names.len().max(1) as u64;
        if self.keep.is_none() && self.count.saturating_mul(cols) > self.max_cells {
            return Err(too_many_cells(self.max_cells));
        }
        let take = match self.keep {
            Some(k) => (k as u64).saturating_sub(start).min(rows as u64) as usize,
            None => rows,
        };
        if take == 0 {
            return Ok(());
        }
        for (i, array) in batch.columns().iter().enumerate() {
            if i >= self.data.len() {
                break;
            }
            let out = &mut self.data[i];
            out.reserve(take);
            let has_time = convert_array(array, take, out)?;
            self.has_time[i] |= has_time;
        }
        Ok(())
    }

    pub fn finish(self) -> (Vec<OutColumn>, u64) {
        let columns = self
            .names
            .into_iter()
            .zip(self.kinds)
            .zip(self.has_time)
            .zip(self.data)
            .map(|(((name, kind), has_time), values)| OutColumn {
                name,
                format: match kind {
                    Kind::Plain => None,
                    Kind::Date => Some(DATE_FORMAT.to_owned()),
                    Kind::DateTime if has_time => Some(DATETIME_FORMAT.to_owned()),
                    Kind::DateTime => Some(DATE_FORMAT.to_owned()),
                    Kind::Time => Some(TIME_FORMAT.to_owned()),
                },
                values,
            })
            .collect();
        (columns, self.count)
    }
}

pub(crate) fn too_many_cells(max: u64) -> RunError {
    RunError::too_large(
        format!(
            "The result has more than {} cells, more than one run may write.",
            group_digits(max)
        ),
        "Aggregate or filter it in the code, or write it in parts.",
    )
}

/// `20000000` → `20,000,000`.
pub fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn kind_of(t: &DataType) -> Kind {
    match t {
        DataType::Date32 | DataType::Date64 => Kind::Date,
        DataType::Timestamp(_, _) => Kind::DateTime,
        DataType::Time32(_) | DataType::Time64(_) => Kind::Time,
        DataType::Dictionary(_, v) => kind_of(v),
        _ => Kind::Plain,
    }
}

fn number(n: f64) -> Value {
    if n.is_nan() {
        Value::Empty
    } else if n.is_infinite() {
        Value::Error(ErrorKind::Num)
    } else {
        Value::Number(n)
    }
}

fn text(s: &str) -> Value {
    if s.is_empty() {
        Value::Empty
    } else {
        Value::Text(Arc::from(s))
    }
}

/// Appends the first `take` values of `array` to `out`. Returns whether any timestamp had a time of day.
fn convert_array(array: &ArrayRef, take: usize, out: &mut Vec<Value>) -> Result<bool, RunError> {
    let mut has_time = false;
    let dt = array.data_type().clone();
    match &dt {
        DataType::Null => out.extend(std::iter::repeat_n(Value::Empty, take)),
        DataType::Boolean => {
            let a = array.as_boolean();
            for i in 0..take {
                out.push(if a.is_null(i) {
                    Value::Empty
                } else {
                    Value::Bool(a.value(i))
                });
            }
        }
        DataType::Int8
        | DataType::Int16
        | DataType::Int32
        | DataType::Int64
        | DataType::UInt8
        | DataType::UInt16
        | DataType::UInt32
        | DataType::UInt64
        | DataType::Float16
        | DataType::Float32
        | DataType::Float64
        | DataType::Decimal32(_, _)
        | DataType::Decimal64(_, _)
        | DataType::Decimal128(_, _)
        | DataType::Decimal256(_, _) => {
            let floats = if matches!(dt, DataType::Float64) {
                array.clone()
            } else {
                cast(array, &DataType::Float64)
                    .map_err(|e| RunError::internal(format!("reading a number column: {e}")))?
            };
            let a = floats.as_primitive::<Float64Type>();
            for i in 0..take {
                out.push(if a.is_null(i) {
                    Value::Empty
                } else {
                    number(a.value(i))
                });
            }
        }
        DataType::Utf8 => {
            let a = array.as_string::<i32>();
            for i in 0..take {
                out.push(if a.is_null(i) {
                    Value::Empty
                } else {
                    text(a.value(i))
                });
            }
        }
        DataType::LargeUtf8 => {
            let a = array.as_string::<i64>();
            for i in 0..take {
                out.push(if a.is_null(i) {
                    Value::Empty
                } else {
                    text(a.value(i))
                });
            }
        }
        DataType::Utf8View => {
            let a = array.as_string_view();
            for i in 0..take {
                out.push(if a.is_null(i) {
                    Value::Empty
                } else {
                    text(a.value(i))
                });
            }
        }
        DataType::Date32 => {
            let a = array.as_primitive::<Date32Type>();
            for i in 0..take {
                out.push(if a.is_null(i) {
                    Value::Empty
                } else {
                    Value::Number(serial_from_unix_days(a.value(i) as i64))
                });
            }
        }
        DataType::Date64 => {
            let a = array.as_primitive::<Date64Type>();
            for i in 0..take {
                out.push(if a.is_null(i) {
                    Value::Empty
                } else {
                    Value::Number(serial_from_unix_micros(a.value(i).saturating_mul(1000)))
                });
            }
        }
        DataType::Timestamp(unit, _) => {
            let micros = |i: usize| -> Option<i64> {
                Some(match unit {
                    TimeUnit::Second => {
                        let a = array.as_primitive::<TimestampSecondType>();
                        if a.is_null(i) {
                            return None;
                        }
                        a.value(i).saturating_mul(1_000_000)
                    }
                    TimeUnit::Millisecond => {
                        let a = array.as_primitive::<TimestampMillisecondType>();
                        if a.is_null(i) {
                            return None;
                        }
                        a.value(i).saturating_mul(1000)
                    }
                    TimeUnit::Microsecond => {
                        let a = array.as_primitive::<TimestampMicrosecondType>();
                        if a.is_null(i) {
                            return None;
                        }
                        a.value(i)
                    }
                    TimeUnit::Nanosecond => {
                        let a = array.as_primitive::<TimestampNanosecondType>();
                        if a.is_null(i) {
                            return None;
                        }
                        a.value(i).div_euclid(1000)
                    }
                })
            };
            for i in 0..take {
                match micros(i) {
                    Some(us) => {
                        if us.rem_euclid(MICROS_PER_DAY) != 0 {
                            has_time = true;
                        }
                        out.push(Value::Number(serial_from_unix_micros(us)));
                    }
                    None => out.push(Value::Empty),
                }
            }
        }
        DataType::Time32(unit) | DataType::Time64(unit) => {
            let micros = |i: usize| -> Option<i64> {
                Some(match (unit, &dt) {
                    (TimeUnit::Second, DataType::Time32(_)) => {
                        let a = array.as_primitive::<Time32SecondType>();
                        if a.is_null(i) {
                            return None;
                        }
                        a.value(i) as i64 * 1_000_000
                    }
                    (TimeUnit::Millisecond, DataType::Time32(_)) => {
                        let a = array.as_primitive::<Time32MillisecondType>();
                        if a.is_null(i) {
                            return None;
                        }
                        a.value(i) as i64 * 1000
                    }
                    (TimeUnit::Microsecond, _) => {
                        let a = array.as_primitive::<Time64MicrosecondType>();
                        if a.is_null(i) {
                            return None;
                        }
                        a.value(i)
                    }
                    _ => {
                        let a = array.as_primitive::<Time64NanosecondType>();
                        if a.is_null(i) {
                            return None;
                        }
                        a.value(i) / 1000
                    }
                })
            };
            for i in 0..take {
                out.push(match micros(i) {
                    Some(us) => Value::Number(us as f64 / MICROS_PER_DAY as f64),
                    None => Value::Empty,
                });
            }
        }
        DataType::Dictionary(_, value_type) => {
            let plain = cast(array, value_type)
                .map_err(|e| RunError::internal(format!("reading a column: {e}")))?;
            has_time = convert_array(&plain, take, out)?;
        }
        _ => {
            // Lists, structs, maps, intervals, binary: their text.
            let opts = FormatOptions::default().with_null("");
            match ArrayFormatter::try_new(array.as_ref(), &opts) {
                Ok(f) => {
                    for i in 0..take {
                        out.push(if array.is_null(i) {
                            Value::Empty
                        } else {
                            text(&f.value(i).to_string())
                        });
                    }
                }
                Err(_) => out.extend(std::iter::repeat_n(Value::Error(ErrorKind::Value), take)),
            }
        }
    }
    Ok(has_time)
}

/// A value as an answer shows it: numbers, text, true/false, null; errors as their text; numbers in a date or
/// time column as ISO text (`2026-10-17`, `2026-10-17 13:45:00`, `13:45:00`).
pub fn answer_json(v: &Value, format: Option<&str>) -> serde_json::Value {
    match v {
        Value::Empty => serde_json::Value::Null,
        Value::Number(n) => match format {
            Some(f) if f == DATE_FORMAT => serde_json::Value::String(iso_date(*n)),
            Some(f) if f == DATETIME_FORMAT => {
                serde_json::Value::String(iso_datetime(*n).replace('T', " "))
            }
            Some(f) if f == TIME_FORMAT => serde_json::Value::String(iso_time(*n)),
            Some(f) if numfmt::is_date_format(f) => {
                serde_json::Value::String(numfmt::format_value(v, Some(f)).0)
            }
            _ => serde_json::Number::from_f64(*n)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
        },
        Value::Text(s) => serde_json::Value::String(s.to_string()),
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Error(e) => serde_json::Value::String(e.as_str().to_owned()),
    }
}

/// A JSON cell as code wrote it: numbers, text, true/false, null; `{"$e": "#N/A"}` is an error and
/// `{"$d": "2026-10-17T…"}` (or `{"$d": <ms since 1970>}`) a date. Anything else becomes its JSON text.
/// Returns the value and whether it was a date.
pub fn value_from_json(j: &serde_json::Value) -> (Value, bool) {
    match j {
        serde_json::Value::Null => (Value::Empty, false),
        serde_json::Value::Bool(b) => (Value::Bool(*b), false),
        serde_json::Value::Number(n) => (
            n.as_f64()
                .map(number)
                .unwrap_or(Value::Error(ErrorKind::Num)),
            false,
        ),
        serde_json::Value::String(s) => (text(s), false),
        serde_json::Value::Object(o) => {
            if let Some(e) = o.get("$e").and_then(|e| e.as_str()) {
                return (
                    Value::Error(ErrorKind::parse(e).unwrap_or(ErrorKind::Value)),
                    false,
                );
            }
            match o.get("$d") {
                Some(serde_json::Value::Number(ms)) => match ms.as_f64() {
                    Some(ms) if ms.is_finite() => (
                        Value::Number(serial_from_unix_micros((ms * 1000.0).round() as i64)),
                        true,
                    ),
                    _ => (Value::Error(ErrorKind::Num), false),
                },
                Some(serde_json::Value::String(s)) => match serial_from_iso(s) {
                    Some(serial) => (Value::Number(serial), true),
                    None => (text(s), false),
                },
                _ => (text(&j.to_string()), false),
            }
        }
        serde_json::Value::Array(_) => (text(&j.to_string()), false),
    }
}

/// A value as code reading a cell gets it, with its kind: `number`, `text`, `bool`, `empty`, `error`, or `date`,
/// `datetime`, `time` when the cell's format shows one (then the value is ISO text).
pub fn cell_json(v: &Value, num_fmt: Option<&str>) -> (serde_json::Value, &'static str) {
    match v {
        Value::Empty => (serde_json::Value::Null, "empty"),
        Value::Number(n) => {
            if let Some(f) = num_fmt.filter(|f| numfmt::is_date_format(f)) {
                let (date, time) = date_parts(f);
                if date && time {
                    return (serde_json::Value::String(iso_datetime(*n)), "datetime");
                }
                if date {
                    return (serde_json::Value::String(iso_date(*n)), "date");
                }
                if *n >= 0.0 && *n < 1.0 {
                    return (serde_json::Value::String(iso_time(*n)), "time");
                }
                return (serde_json::Value::String(iso_datetime(*n)), "datetime");
            }
            (
                serde_json::Number::from_f64(*n)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null),
                "number",
            )
        }
        Value::Text(s) => (serde_json::Value::String(s.to_string()), "text"),
        Value::Bool(b) => (serde_json::Value::Bool(*b), "bool"),
        Value::Error(e) => (serde_json::Value::String(e.as_str().to_owned()), "error"),
    }
}

/// Whether an Excel date format code shows a date part and a time part.
pub fn date_parts(code: &str) -> (bool, bool) {
    let first = code.split(';').next().unwrap_or("");
    let (mut date, mut time) = (false, false);
    let (mut in_quote, mut in_bracket, mut escaped) = (false, false, false);
    let mut prev = ' ';
    let chars: Vec<char> = first.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '"' => in_quote = !in_quote,
            '[' if !in_quote => {
                in_bracket = true;
                // [h], [mm], [ss]: elapsed time.
                if chars
                    .get(i + 1)
                    .is_some_and(|n| matches!(n.to_ascii_lowercase(), 'h' | 'm' | 's'))
                {
                    time = true;
                }
            }
            ']' if !in_quote => in_bracket = false,
            _ if in_quote || in_bracket => {}
            'y' | 'Y' | 'd' | 'D' => date = true,
            'h' | 'H' | 's' | 'S' => time = true,
            'm' | 'M' => {
                // Minutes right after hours or right before seconds; months otherwise.
                let next_s = chars[i + 1..]
                    .iter()
                    .find(|c| c.is_ascii_alphabetic() && !c.eq_ignore_ascii_case(&'m'))
                    .is_some_and(|c| c.eq_ignore_ascii_case(&'s'));
                if prev.eq_ignore_ascii_case(&'h') || next_s {
                    time = true;
                } else {
                    date = true;
                }
            }
            _ => {}
        }
        if c.is_ascii_alphabetic() && !c.eq_ignore_ascii_case(&'m') {
            prev = c;
        }
    }
    (date, time)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serials() {
        assert_eq!(serial_from_unix_days(0), 25_569.0);
        assert_eq!(unix_days_from_serial(25_569.0), 0);
        // 1900-03-01 is serial 61; 1900-02-28 is 59.
        assert_eq!(
            serial_from_unix_days(unix_days_from_civil(1900, 3, 1)),
            61.0
        );
        assert_eq!(
            serial_from_unix_days(unix_days_from_civil(1900, 2, 28)),
            59.0
        );
        assert_eq!(
            unix_days_from_serial(59.0),
            unix_days_from_civil(1900, 2, 28)
        );
        assert_eq!(iso_date(46_312.0), "2026-10-17");
        assert_eq!(iso_datetime(46_312.5), "2026-10-17T12:00:00");
        assert_eq!(iso_time(0.75), "18:00:00");
        assert_eq!(serial_from_iso("2026-10-17"), Some(46_312.0));
        assert_eq!(serial_from_iso("2026-10-17T12:00:00Z"), Some(46_312.5));
        assert_eq!(serial_from_iso("2026-10-17 14:00:00+02:00"), Some(46_312.5));
        assert_eq!(
            unix_micros_from_serial(serial_from_unix_micros(1_760_000_000_123_456)),
            1_760_000_000_123_456
        );
    }

    #[test]
    fn format_parts() {
        assert_eq!(date_parts("yyyy-mm-dd"), (true, false));
        assert_eq!(date_parts("yyyy-mm-dd hh:mm:ss"), (true, true));
        assert_eq!(date_parts("h:mm"), (false, true));
        assert_eq!(date_parts("mm:ss"), (false, true));
        assert_eq!(date_parts("mmm yyyy"), (true, false));
        assert_eq!(date_parts("[h]:mm:ss"), (false, true));
        assert_eq!(date_parts("d \"days\""), (true, false));
    }

    #[test]
    fn digits() {
        assert_eq!(group_digits(20_000_000), "20,000,000");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1000), "1,000");
    }
}
