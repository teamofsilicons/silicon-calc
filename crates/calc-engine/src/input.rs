//! What a typed entry becomes: a formula, a number (with the format its spelling implies), a date, a logical,
//! an error, or text.

use crate::{dates, value::ErrorKind};

/// A parsed entry.
#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    Empty,
    /// `=…`: the text after the equals sign.
    Formula(String),
    /// A number and, when the spelling implies one ("12%", "$5", "2026-10-17"), the format to give the cell.
    Number(f64, Option<String>),
    Bool(bool),
    Error(ErrorKind),
    Text(String),
}

/// Parses what someone typed into a cell, as Excel does in en-US.
pub fn parse_entry(raw: &str) -> Entry {
    if raw.is_empty() {
        return Entry::Empty;
    }
    if let Some(rest) = raw.strip_prefix('\'') {
        return Entry::Text(rest.to_owned());
    }
    if raw.len() > 1 && (raw.starts_with('=') || (raw.starts_with('+') && !looks_numeric(raw))) {
        return Entry::Formula(raw[1..].to_owned());
    }
    let t = raw.trim();
    if t.eq_ignore_ascii_case("TRUE") {
        return Entry::Bool(true);
    }
    if t.eq_ignore_ascii_case("FALSE") {
        return Entry::Bool(false);
    }
    if let Some(e) = ErrorKind::parse(t) {
        return Entry::Error(e);
    }
    if let Some((n, fmt)) = parse_number_literal(t) {
        return Entry::Number(n, fmt);
    }
    if let Some((serial, shape)) = dates::parse_date_time(t) {
        return Entry::Number(serial, Some(shape.format_code().to_owned()));
    }
    Entry::Text(raw.to_owned())
}

fn looks_numeric(s: &str) -> bool {
    parse_number_literal(s.trim()).is_some()
}

/// A number as typed: "12", "-1.5", "1e3", "1,234.5", "50%", "$1,234.50", "(42)", "-$3".
/// Returns the number and the format its spelling implies.
pub fn parse_number_literal(t: &str) -> Option<(f64, Option<String>)> {
    let mut s = t.trim();
    if s.is_empty() || s.len() > 64 {
        return None;
    }
    let mut negative = false;
    if s.starts_with('(') && s.ends_with(')') && s.len() > 2 {
        negative = true;
        s = &s[1..s.len() - 1];
    }
    if let Some(r) = s.strip_prefix('-') {
        negative = !negative;
        s = r;
    } else if let Some(r) = s.strip_prefix('+') {
        s = r;
    }
    let mut currency = false;
    if let Some(r) = s.strip_prefix('$') {
        currency = true;
        s = r;
    }
    if let Some(r) = s.strip_prefix('-')
        && currency
    {
        negative = !negative;
        s = r;
    }
    let mut percent = false;
    if let Some(r) = s.strip_suffix('%') {
        percent = true;
        s = r.trim_end();
    }
    if s.is_empty() {
        return None;
    }
    let grouped = s.contains(',');
    if grouped {
        // Thousands separators must be in the right places: 1,234,567.89
        let int_part = s.split(['.', 'e', 'E']).next().unwrap_or("");
        let groups: Vec<&str> = int_part.split(',').collect();
        if groups[0].is_empty() || groups[0].len() > 3 || groups[1..].iter().any(|g| g.len() != 3) {
            return None;
        }
    }
    let plain: String = s.chars().filter(|&c| c != ',').collect();
    let bytes = plain.as_bytes();
    if !bytes.iter().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-')) {
        return None;
    }
    if !bytes[0].is_ascii_digit() && bytes[0] != b'.' {
        return None;
    }
    if plain.eq_ignore_ascii_case("inf") || plain.eq_ignore_ascii_case("nan") {
        return None;
    }
    let mut n: f64 = plain.parse().ok()?;
    if !n.is_finite() {
        return None;
    }
    if negative {
        n = -n;
    }
    let decimals = plain.split_once('.').map(|(_, f)| f.chars().take_while(char::is_ascii_digit).count()).unwrap_or(0);
    let scientific = plain.contains(['e', 'E']);
    let fmt = if percent {
        n /= 100.0;
        Some(if decimals > 0 { format!("0.{}%", "0".repeat(decimals)) } else { "0%".to_owned() })
    } else if currency {
        Some(if decimals > 0 { "$#,##0.00".to_owned() } else { "$#,##0".to_owned() })
    } else if scientific {
        Some("0.00E+00".to_owned())
    } else if grouped {
        Some(if decimals > 0 { format!("#,##0.{}", "0".repeat(decimals)) } else { "#,##0".to_owned() })
    } else {
        None
    };
    Some((n, fmt))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries() {
        assert_eq!(parse_entry("=A1+1"), Entry::Formula("A1+1".into()));
        assert_eq!(parse_entry("12"), Entry::Number(12.0, None));
        assert_eq!(parse_entry("-1.5"), Entry::Number(-1.5, None));
        assert_eq!(parse_entry("50%"), Entry::Number(0.5, Some("0%".into())));
        assert_eq!(parse_entry("$1,234.50"), Entry::Number(1234.5, Some("$#,##0.00".into())));
        assert_eq!(parse_entry("(42)"), Entry::Number(-42.0, None));
        assert_eq!(parse_entry("1,23"), Entry::Text("1,23".into()));
        assert_eq!(parse_entry("true"), Entry::Bool(true));
        assert_eq!(parse_entry("#N/A"), Entry::Error(ErrorKind::NA));
        assert_eq!(parse_entry("'=x"), Entry::Text("=x".into()));
        assert_eq!(parse_entry("hello"), Entry::Text("hello".into()));
        assert_eq!(parse_entry("2026-10-17"), Entry::Number(46312.0, Some("yyyy-mm-dd".into())));
        assert_eq!(parse_entry("+5"), Entry::Number(5.0, None));
        assert_eq!(parse_entry("1e3"), Entry::Number(1000.0, Some("0.00E+00".into())));
        assert_eq!(parse_entry("e3"), Entry::Text("e3".into()));
    }
}
