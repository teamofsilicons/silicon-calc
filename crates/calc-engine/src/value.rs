//! Cell values and the errors formulas produce, with Excel's comparison and coercion rules.

use std::{cmp::Ordering, fmt, sync::Arc};

use serde::{Deserialize, Serialize};

/// An Excel error value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum ErrorKind {
    Null = 1,
    Div0 = 2,
    Value = 3,
    Ref = 4,
    Name = 5,
    Num = 6,
    NA = 7,
    Spill = 8,
    Calc = 9,
    /// A circular reference. Excel shows 0 with a warning; Calc shows the error so the cycle is visible.
    Circ = 10,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Null => "#NULL!",
            Self::Div0 => "#DIV/0!",
            Self::Value => "#VALUE!",
            Self::Ref => "#REF!",
            Self::Name => "#NAME?",
            Self::Num => "#NUM!",
            Self::NA => "#N/A",
            Self::Spill => "#SPILL!",
            Self::Calc => "#CALC!",
            Self::Circ => "#CIRC!",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_uppercase().as_str() {
            "#NULL!" => Self::Null,
            "#DIV/0!" => Self::Div0,
            "#VALUE!" => Self::Value,
            "#REF!" => Self::Ref,
            "#NAME?" => Self::Name,
            "#NUM!" => Self::Num,
            "#N/A" => Self::NA,
            "#SPILL!" => Self::Spill,
            "#CALC!" => Self::Calc,
            "#CIRC!" => Self::Circ,
            _ => return None,
        })
    }

    pub fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Null,
            2 => Self::Div0,
            3 => Self::Value,
            4 => Self::Ref,
            5 => Self::Name,
            6 => Self::Num,
            7 => Self::NA,
            8 => Self::Spill,
            9 => Self::Calc,
            _ => Self::Circ,
        }
    }

    /// The number ERROR.TYPE returns.
    pub fn type_number(self) -> f64 {
        match self {
            Self::Null => 1.0,
            Self::Div0 => 2.0,
            Self::Value => 3.0,
            Self::Ref => 4.0,
            Self::Name => 5.0,
            Self::Num => 6.0,
            Self::NA => 7.0,
            Self::Spill => 9.0,
            Self::Calc => 14.0,
            Self::Circ => 3.0,
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A value a cell holds or a formula produces.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "lowercase")]
pub enum Value {
    #[default]
    Empty,
    Number(f64),
    Text(Arc<str>),
    Bool(bool),
    Error(ErrorKind),
}

impl Value {
    pub fn text(s: impl Into<Arc<str>>) -> Self {
        Self::Text(s.into())
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error(_))
    }

    pub fn as_number(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// Excel's rule for a value used where a number is expected (`=A1+1`).
    pub fn coerce_number(&self) -> Result<f64, ErrorKind> {
        match self {
            Self::Empty => Ok(0.0),
            Self::Number(n) => Ok(*n),
            Self::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
            Self::Text(s) => parse_number_text(s).ok_or(ErrorKind::Value),
            Self::Error(e) => Err(*e),
        }
    }

    /// Excel's rule for a value used where text is expected (`=A1&"x"`).
    pub fn coerce_text(&self) -> Result<Arc<str>, ErrorKind> {
        match self {
            Self::Empty => Ok(Arc::from("")),
            Self::Number(n) => Ok(Arc::from(format_general(*n).as_str())),
            Self::Text(s) => Ok(s.clone()),
            Self::Bool(b) => Ok(Arc::from(if *b { "TRUE" } else { "FALSE" })),
            Self::Error(e) => Err(*e),
        }
    }

    /// Excel's rule for a value used where a logical is expected (`=IF(A1, …)`).
    pub fn coerce_bool(&self) -> Result<bool, ErrorKind> {
        match self {
            Self::Empty => Ok(false),
            Self::Number(n) => Ok(*n != 0.0),
            Self::Bool(b) => Ok(*b),
            Self::Text(s) => {
                if s.eq_ignore_ascii_case("TRUE") {
                    Ok(true)
                } else if s.eq_ignore_ascii_case("FALSE") {
                    Ok(false)
                } else {
                    Err(ErrorKind::Value)
                }
            }
            Self::Error(e) => Err(*e),
        }
    }

    /// The text a cell shows with the General format.
    pub fn display(&self) -> String {
        match self {
            Self::Empty => String::new(),
            Self::Number(n) => format_general(*n),
            Self::Text(s) => s.to_string(),
            Self::Bool(b) => (if *b { "TRUE" } else { "FALSE" }).to_owned(),
            Self::Error(e) => e.as_str().to_owned(),
        }
    }

    fn type_rank(&self) -> u8 {
        // Excel orders numbers < text < logicals; errors and blanks sort last.
        match self {
            Self::Number(_) => 0,
            Self::Text(_) => 1,
            Self::Bool(_) => 2,
            Self::Error(_) => 3,
            Self::Empty => 4,
        }
    }

    /// The order SORT and the sort operation use (ascending), with blanks last.
    pub fn sort_cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Number(a), Self::Number(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
            (Self::Text(a), Self::Text(b)) => cmp_text_ci(a, b),
            (Self::Bool(a), Self::Bool(b)) => a.cmp(b),
            _ => self.type_rank().cmp(&other.type_rank()),
        }
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Self {
        Self::Number(n)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Self::Text(Arc::from(s))
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Self::Text(Arc::from(s.as_str()))
    }
}

/// Case-insensitive text comparison, the way Excel compares text.
pub fn cmp_text_ci(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().flat_map(char::to_lowercase);
    let mut bi = b.chars().flat_map(char::to_lowercase);
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => match x.cmp(&y) {
                Ordering::Equal => {}
                other => return other,
            },
        }
    }
}

/// Comparison for `=`, `<`, `>` and friends. Blank compares as 0, "" or FALSE depending on the other side.
pub fn compare(a: &Value, b: &Value) -> Ordering {
    use Value::*;
    let a = match (a, b) {
        (Empty, Number(_)) => &Number(0.0),
        (Empty, Text(_)) => &Value::Text(Arc::from("")),
        (Empty, Bool(_)) => &Bool(false),
        _ => a,
    };
    let b = match (a, b) {
        (Number(_), Empty) => &Number(0.0),
        (Text(_), Empty) => &Value::Text(Arc::from("")),
        (Bool(_), Empty) => &Bool(false),
        _ => b,
    };
    match (a, b) {
        (Number(x), Number(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Text(x), Text(y)) => cmp_text_ci(x, y),
        (Bool(x), Bool(y)) => x.cmp(y),
        (Empty, Empty) => Ordering::Equal,
        _ => a.type_rank().cmp(&b.type_rank()),
    }
}

/// Text Excel accepts as a number when coercing: "12", " 1.5e3 ", "50%", "$1,200", "(4)".
pub fn parse_number_text(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(v) = crate::input::parse_number_literal(t) {
        return Some(v.0);
    }
    if let Some((serial, _)) = crate::dates::parse_date_time(t) {
        return Some(serial);
    }
    None
}

/// Excel's General format: up to 11 significant digits in a cell this wide, scientific for very large or small.
pub fn format_general(n: f64) -> String {
    if n.is_nan() {
        return "#NUM!".to_owned();
    }
    if n == 0.0 {
        return "0".to_owned();
    }
    let abs = n.abs();
    if !(1e-9..1e11).contains(&abs) {
        // Scientific with up to 6 significant digits after the point, trimmed.
        let s = format!("{:.5E}", n);
        let (mant, exp) = s.split_once('E').unwrap_or((&s, "0"));
        let mant = trim_fraction(mant);
        let exp: i32 = exp.parse().unwrap_or(0);
        let sign = if exp < 0 { '-' } else { '+' };
        return format!("{mant}E{sign}{:02}", exp.abs());
    }
    // 10 significant digits after rounding, like a default-width Excel cell (and Google Sheets).
    let digits_before = if abs >= 1.0 { (abs.log10().floor() as i32) + 1 } else { 0 };
    let decimals = (10 - digits_before).clamp(0, 15) as usize;
    let s = format!("{:.*}", decimals, n);
    let s = trim_fraction(&s);
    if s == "-0" { "0".to_owned() } else { s }
}

fn trim_fraction(s: &str) -> String {
    if s.contains('.') {
        let t = s.trim_end_matches('0');
        t.trim_end_matches('.').to_owned()
    } else {
        s.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn general_format() {
        assert_eq!(format_general(1.0), "1");
        assert_eq!(format_general(0.1 + 0.2), "0.3");
        assert_eq!(format_general(1234567.891), "1234567.891");
        assert_eq!(format_general(-2.5), "-2.5");
        assert_eq!(format_general(1e12), "1E+12");
        assert_eq!(format_general(1.5e-12), "1.5E-12");
        assert_eq!(format_general(1.0 / 3.0), "0.3333333333");
    }

    #[test]
    fn comparison() {
        assert_eq!(compare(&Value::Empty, &Value::Number(0.0)), Ordering::Equal);
        assert_eq!(compare(&Value::from("a"), &Value::from("A")), Ordering::Equal);
        assert_eq!(compare(&Value::Number(5.0), &Value::from("1")), Ordering::Less);
    }
}
