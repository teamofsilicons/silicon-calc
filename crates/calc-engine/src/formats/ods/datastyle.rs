//! Number formats between OpenDocument data styles (`number:number-style`, `number:date-style`, …) and Excel
//! format codes, which is what the engine keeps.
//!
//! Reading turns a data style (with the conditional sections its `style:map`s point at) into one Excel code:
//! LibreOffice writes `#,##0.00;[Red]-#,##0.00` as a style for the negative section that maps `value()>=0` to a
//! second style for the positive one, and that comes back as the same code. Writing goes the other way, one ODF
//! style per section.

use std::io::BufRead;

use ahash::HashMap;
use quick_xml::{
    Reader,
    events::{BytesStart, Event},
};

use crate::formats::xml::{attr, attr_f64, attr_u32, escape, local, name_of, push_text, truthy};

/// The kinds of data style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StyleKind {
    Number,
    Currency,
    Percentage,
    Date,
    Time,
    Boolean,
    Text,
}

impl StyleKind {
    /// From a data style element's local name.
    pub fn of_element(local_name: &[u8]) -> Option<Self> {
        Some(match local_name {
            b"number-style" => Self::Number,
            b"currency-style" => Self::Currency,
            b"percentage-style" => Self::Percentage,
            b"date-style" => Self::Date,
            b"time-style" => Self::Time,
            b"boolean-style" => Self::Boolean,
            b"text-style" => Self::Text,
            _ => return None,
        })
    }

    fn element(self) -> &'static str {
        match self {
            Self::Number => "number:number-style",
            Self::Currency => "number:currency-style",
            Self::Percentage => "number:percentage-style",
            Self::Date => "number:date-style",
            Self::Time => "number:time-style",
            Self::Boolean => "number:boolean-style",
            Self::Text => "number:text-style",
        }
    }
}

/// One piece of a data style.
#[derive(Clone, Debug, PartialEq)]
enum Part {
    Number {
        /// None: as many as the number needs (General).
        decimals: Option<u32>,
        min_decimals: Option<u32>,
        min_int: u32,
        grouping: bool,
        /// Shown divided by this (1000 for Excel's trailing `,`).
        factor: f64,
        /// Text inside the integer digits, with how many digits stand to its right (`000-00-0000`).
        embedded: Vec<(u32, String)>,
    },
    Scientific {
        decimals: u32,
        min_int: u32,
        min_exp: u32,
        /// 3 for engineering notation (`##0.0E+0`).
        interval: u32,
    },
    Fraction {
        /// None: no whole part (`?/?`).
        min_int: Option<u32>,
        num: u32,
        den: u32,
        den_value: Option<u32>,
    },
    Text(String),
    Currency(String),
    Year(bool),
    Month {
        long: bool,
        textual: bool,
    },
    Day(bool),
    DayOfWeek(bool),
    Hours(bool),
    Minutes(bool),
    Seconds {
        long: bool,
        decimals: u32,
    },
    AmPm,
    TextContent,
    Boolean,
}

/// A data style as a file has it.
#[derive(Clone, Debug)]
pub(crate) struct DataStyle {
    pub kind: StyleKind,
    parts: Vec<Part>,
    /// The colour its text-properties give (`[Red]`).
    color: Option<String>,
    /// `(condition, style name)`: other sections, by the condition that picks them.
    maps: Vec<(String, String)>,
    /// Hours (or minutes) past their usual range: `[h]:mm:ss`.
    elapsed: bool,
}

// ---- reading -----------------------------------------------------------------------------------------------------

/// Reads the data style element just started (`empty` when it was `<…/>`) and returns its name and contents.
pub(crate) fn read<R: BufRead>(r: &mut Reader<R>, start: &BytesStart<'_>, kind: StyleKind, empty: bool) -> (String, DataStyle) {
    let name = attr(start, b"name").unwrap_or_default();
    let elapsed = attr(start, b"truncate-on-overflow").is_some_and(|v| !truthy(&v));
    let mut ds = DataStyle { kind, parts: Vec::new(), color: None, maps: Vec::new(), elapsed };
    if empty {
        return (name, ds);
    }
    let mut buf = Vec::new();
    let mut depth = 1usize;
    // The element whose text is being collected (number:text, number:currency-symbol, number:embedded-text).
    let mut collecting: Option<(&'static str, usize, String, u32)> = None;
    loop {
        buf.clear();
        let ev = match r.read_event_into(&mut buf) {
            Ok(ev) => ev,
            Err(_) => break,
        };
        match &ev {
            Event::Start(e) | Event::Empty(e) => {
                let is_start = matches!(ev, Event::Start(_));
                if is_start {
                    depth += 1;
                }
                let n = name_of(e);
                let long = attr(e, b"style").is_some_and(|v| v == "long");
                match n {
                    b"number" => ds.parts.push(Part::Number {
                        decimals: attr_u32(e, b"decimal-places"),
                        min_decimals: attr_u32(e, b"min-decimal-places"),
                        min_int: attr_u32(e, b"min-integer-digits").unwrap_or(0),
                        grouping: attr(e, b"grouping").is_some_and(|v| truthy(&v)),
                        factor: attr_f64(e, b"display-factor").filter(|f| *f > 0.0).unwrap_or(1.0),
                        embedded: Vec::new(),
                    }),
                    b"scientific-number" => ds.parts.push(Part::Scientific {
                        decimals: attr_u32(e, b"decimal-places").unwrap_or(0),
                        min_int: attr_u32(e, b"min-integer-digits").unwrap_or(1),
                        min_exp: attr_u32(e, b"min-exponent-digits").unwrap_or(2),
                        interval: attr_u32(e, b"exponent-interval").unwrap_or(1),
                    }),
                    b"fraction" => ds.parts.push(Part::Fraction {
                        min_int: attr_u32(e, b"min-integer-digits"),
                        num: attr_u32(e, b"min-numerator-digits").unwrap_or(1).max(1),
                        den: attr_u32(e, b"min-denominator-digits").unwrap_or(1).max(1),
                        den_value: attr_u32(e, b"denominator-value"),
                    }),
                    b"text" if is_start => collecting = Some(("text", depth, String::new(), 0)),
                    b"currency-symbol" if is_start => collecting = Some(("currency", depth, String::new(), 0)),
                    b"embedded-text" if is_start => {
                        collecting = Some(("embedded", depth, String::new(), attr_u32(e, b"position").unwrap_or(0)))
                    }
                    b"fill-character" if is_start => collecting = Some(("skip", depth, String::new(), 0)),
                    b"text-properties" => {
                        if let Some(c) = attr(e, b"color") {
                            ds.color = Some(c.to_ascii_uppercase());
                        }
                    }
                    b"map" => {
                        if let (Some(c), Some(s)) = (attr(e, b"condition"), attr(e, b"apply-style-name")) {
                            ds.maps.push((c, s));
                        }
                    }
                    b"year" => ds.parts.push(Part::Year(long)),
                    b"month" => ds.parts.push(Part::Month { long, textual: attr(e, b"textual").is_some_and(|v| truthy(&v)) }),
                    b"day" => ds.parts.push(Part::Day(long)),
                    b"day-of-week" => ds.parts.push(Part::DayOfWeek(long)),
                    b"hours" => ds.parts.push(Part::Hours(long)),
                    b"minutes" => ds.parts.push(Part::Minutes(long)),
                    b"seconds" => {
                        ds.parts.push(Part::Seconds { long, decimals: attr_u32(e, b"decimal-places").unwrap_or(0) })
                    }
                    b"am-pm" => ds.parts.push(Part::AmPm),
                    b"text-content" => ds.parts.push(Part::TextContent),
                    b"boolean" => ds.parts.push(Part::Boolean),
                    _ => {}
                }
            }
            Event::End(_) => {
                if let Some((what, d, text, pos)) = collecting.take() {
                    if d == depth {
                        match what {
                            "text" => ds.parts.push(Part::Text(text)),
                            "currency" => ds.parts.push(Part::Currency(text)),
                            "embedded" => {
                                if let Some(Part::Number { embedded, .. }) = ds.parts.last_mut() {
                                    embedded.push((pos, text));
                                }
                            }
                            _ => {}
                        }
                    } else {
                        collecting = Some((what, d, text, pos));
                    }
                }
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            Event::Eof => break,
            other => {
                if let Some((_, _, text, _)) = collecting.as_mut() {
                    push_text(text, other);
                }
            }
        }
    }
    (name, ds)
}

/// Excel's colour name for an ODF colour, when it has one.
fn color_name(hex: &str) -> Option<&'static str> {
    Some(match hex {
        "#FF0000" => "Red",
        "#0000FF" => "Blue",
        "#00FF00" => "Green",
        "#000000" => "Black",
        "#FFFFFF" => "White",
        "#FFFF00" => "Yellow",
        "#FF00FF" => "Magenta",
        "#00FFFF" => "Cyan",
        _ => return None,
    })
}

/// Literal text as an Excel code writes it: characters Excel shows as they are stay bare, the rest is quoted.
fn literal(text: &str, out: &mut String, in_date: bool, after_seconds: bool) {
    if text.is_empty() {
        return;
    }
    let bare = |c: char| match c {
        ' ' | '-' | '+' | '(' | ')' | ':' => true,
        '/' | ',' => in_date,
        '.' => in_date && !after_seconds,
        _ => false,
    };
    if text.chars().all(bare) {
        out.push_str(text);
    } else {
        out.push('"');
        out.push_str(&text.replace('"', "\"\""));
        out.push('"');
    }
}

/// The integer digits of a number: `0`, `#,##0`, `000-00-0000`.
fn integer_digits(min_int: u32, grouping: bool, embedded: &[(u32, String)], out: &mut String) {
    let max_pos = embedded.iter().map(|(p, _)| *p).max().unwrap_or(0);
    let width = min_int.max(if grouping { 4 } else { 1 }).max(max_pos + 1);
    for i in (0..width).rev() {
        // Digit i has i digits to its right; text at position p stands left of digit p - 1.
        for (_, text) in embedded.iter().filter(|(p, _)| *p == i + 1) {
            literal(text, out, false, false);
        }
        out.push(if i < min_int { '0' } else { '#' });
        if grouping && i == 3 {
            out.push(',');
        }
    }
}

fn section_code(ds: &DataStyle) -> String {
    let mut out = String::new();
    if let Some(name) = ds.color.as_deref().and_then(color_name) {
        out.push('[');
        out.push_str(name);
        out.push(']');
    }
    let in_date = matches!(ds.kind, StyleKind::Date | StyleKind::Time);
    let mut elapsed_pending = ds.elapsed;
    let mut after_seconds = false;
    let only_text = ds.parts.iter().all(|p| matches!(p, Part::Text(_) | Part::Currency(_)));
    for p in &ds.parts {
        let mut this_seconds = false;
        match p {
            Part::Number { decimals, min_decimals, min_int, grouping, factor, embedded } => match decimals {
                None if !*grouping && embedded.is_empty() && *min_int <= 1 && *factor == 1.0 => out.push_str("General"),
                _ => {
                    integer_digits(*min_int, *grouping, embedded, &mut out);
                    let d = decimals.unwrap_or(0);
                    if d > 0 {
                        let md = min_decimals.unwrap_or(d).min(d);
                        out.push('.');
                        out.extend(std::iter::repeat_n('0', md as usize));
                        out.extend(std::iter::repeat_n('#', (d - md) as usize));
                    }
                    let mut f = *factor;
                    while f >= 999.0 {
                        out.push(',');
                        f /= 1000.0;
                    }
                }
            },
            Part::Scientific { decimals, min_int, min_exp, interval } => {
                if *interval >= 2 {
                    let w = (*interval).max(*min_int);
                    out.extend(std::iter::repeat_n('#', (w - (*min_int).min(w)) as usize));
                    out.extend(std::iter::repeat_n('0', (*min_int).min(w) as usize));
                } else {
                    out.extend(std::iter::repeat_n('0', (*min_int).max(1) as usize));
                }
                if *decimals > 0 {
                    out.push('.');
                    out.extend(std::iter::repeat_n('0', *decimals as usize));
                }
                out.push_str("E+");
                out.extend(std::iter::repeat_n('0', (*min_exp).max(1) as usize));
            }
            Part::Fraction { min_int, num, den, den_value } => {
                if let Some(mi) = min_int {
                    if *mi == 0 {
                        out.push('#');
                    } else {
                        out.extend(std::iter::repeat_n('0', *mi as usize));
                    }
                    out.push(' ');
                }
                out.extend(std::iter::repeat_n('?', *num as usize));
                out.push('/');
                match den_value {
                    Some(v) if *v > 0 => out.push_str(&v.to_string()),
                    _ => out.extend(std::iter::repeat_n('?', *den as usize)),
                }
            }
            Part::Text(t) => {
                if ds.kind == StyleKind::Percentage && t.trim() == "%" {
                    out.push_str(t);
                } else if only_text {
                    // A section that is only text (`"-"` for zero) is quoted whatever it holds.
                    out.push('"');
                    out.push_str(&t.replace('"', "\"\""));
                    out.push('"');
                } else {
                    literal(t, &mut out, in_date, after_seconds);
                }
            }
            Part::Currency(sym) => {
                out.push('"');
                out.push_str(&sym.replace('"', "\"\""));
                out.push('"');
            }
            Part::Year(long) => out.push_str(if *long { "yyyy" } else { "yy" }),
            Part::Month { long, textual } => out.push_str(match (textual, long) {
                (true, true) => "mmmm",
                (true, false) => "mmm",
                (false, true) => "mm",
                (false, false) => "m",
            }),
            Part::Day(long) => out.push_str(if *long { "dd" } else { "d" }),
            Part::DayOfWeek(long) => out.push_str(if *long { "dddd" } else { "ddd" }),
            Part::Hours(long) => {
                let h = if *long { "hh" } else { "h" };
                if elapsed_pending {
                    elapsed_pending = false;
                    out.push('[');
                    out.push_str(h);
                    out.push(']');
                } else {
                    out.push_str(h);
                }
            }
            Part::Minutes(long) => {
                let m = if *long { "mm" } else { "m" };
                if elapsed_pending {
                    elapsed_pending = false;
                    out.push('[');
                    out.push_str(m);
                    out.push(']');
                } else {
                    out.push_str(m);
                }
            }
            Part::Seconds { long, decimals } => {
                out.push_str(if *long { "ss" } else { "s" });
                if *decimals > 0 {
                    out.push('.');
                    out.extend(std::iter::repeat_n('0', *decimals as usize));
                }
                this_seconds = true;
            }
            Part::AmPm => out.push_str("AM/PM"),
            Part::TextContent => out.push('@'),
            Part::Boolean => {}
        }
        after_seconds = this_seconds;
    }
    out
}

/// `value()>=0` as `(">=", 0.0)`.
fn parse_condition(c: &str) -> Option<(String, f64)> {
    let rest = c.trim().strip_prefix("value()")?.trim();
    for op in [">=", "<=", "!=", "<>", ">", "<", "=="] {
        if let Some(v) = rest.strip_prefix(op) {
            let op = match op {
                "!=" | "<>" => "<>",
                "==" => "=",
                o => o,
            };
            return Some((op.to_owned(), v.trim().parse().ok()?));
        }
    }
    let v = rest.strip_prefix('=')?;
    Some(("=".to_owned(), v.trim().parse().ok()?))
}

fn number_text(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 { format!("{}", n as i64) } else { format!("{n}") }
}

/// The Excel code for the data style named `name` (with its conditional sections), or None for General and
/// for styles that show values as they are (booleans).
pub(crate) fn excel_code(name: &str, styles: &HashMap<String, DataStyle>) -> Option<String> {
    let ds = styles.get(name)?;
    if ds.kind == StyleKind::Boolean {
        return None;
    }
    let main = section_code(ds);
    let mut sections: Vec<String> = Vec::new();
    let mapped: Vec<(Option<(String, f64)>, &DataStyle)> =
        ds.maps.iter().filter_map(|(c, s)| Some((parse_condition(c), styles.get(s)?))).take(2).collect();
    let plain = |c: &Option<(String, f64)>, op: &str| c.as_ref().is_some_and(|(o, v)| o == op && *v == 0.0);
    match mapped.as_slice() {
        [] => {}
        [(c, s)] if plain(c, ">=") || plain(c, ">") => sections.push(section_code(s)),
        [(c0, s0), (c1, s1)] if plain(c0, ">") && plain(c1, "<") => {
            sections.push(section_code(s0));
            sections.push(section_code(s1));
        }
        list => {
            for (c, s) in list {
                let code = section_code(s);
                match c {
                    Some((op, v)) => sections.push(format!("[{op}{}]{code}", number_text(*v))),
                    None => sections.push(code),
                }
            }
        }
    }
    sections.push(main);
    let code = sections.join(";");
    let trimmed = code.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("general") { None } else { Some(code) }
}

// ---- writing -----------------------------------------------------------------------------------------------------

/// A piece of an Excel format code.
#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Lit(String),
    /// A run of digit placeholders and the separators between them: `#,##0.00`, `0`, `??`.
    Digits(String),
    Percent,
    Exp(u32),
    Slash,
    Year(usize),
    Month(usize),
    Minute(usize),
    Day(usize),
    Hour(usize),
    Second(usize),
    SubSecond(usize),
    ElapsedHour(usize),
    ElapsedMinute(usize),
    ElapsedSecond(usize),
    AmPm,
    At,
    General,
}

/// One section of a code: its tokens, colour and condition.
#[derive(Default, Debug)]
struct Section {
    toks: Vec<Tok>,
    color: Option<&'static str>,
    condition: Option<(String, String)>,
}

fn split_sections(code: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut in_quote = false;
    let mut escape_next = false;
    for c in code.chars() {
        let cur = out.last_mut().expect("a section");
        if escape_next {
            cur.push(c);
            escape_next = false;
            continue;
        }
        match c {
            '\\' => {
                escape_next = true;
                cur.push(c);
            }
            '"' => {
                in_quote = !in_quote;
                cur.push(c);
            }
            ';' if !in_quote => out.push(String::new()),
            _ => cur.push(c),
        }
    }
    out
}

fn tokenize(sec: &str) -> Section {
    let chars: Vec<char> = sec.chars().collect();
    let mut s = Section::default();
    let mut i = 0;
    let push_lit = |toks: &mut Vec<Tok>, t: &str| {
        if let Some(Tok::Lit(prev)) = toks.last_mut() {
            prev.push_str(t);
        } else {
            toks.push(Tok::Lit(t.to_owned()));
        }
    };
    let run = |chars: &[char], i: usize, c: char| chars[i..].iter().take_while(|x| x.eq_ignore_ascii_case(&c)).count();
    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' => {
                let mut j = i + 1;
                let mut text = String::new();
                while j < chars.len() && chars[j] != '"' {
                    text.push(chars[j]);
                    j += 1;
                }
                push_lit(&mut s.toks, &text);
                i = j + 1;
            }
            '\\' => {
                if let Some(n) = chars.get(i + 1) {
                    push_lit(&mut s.toks, &n.to_string());
                }
                i += 2;
            }
            '_' => {
                push_lit(&mut s.toks, " ");
                i += 2;
            }
            '*' => i += 2,
            '[' => {
                let end = chars[i..].iter().position(|&x| x == ']').map(|p| i + p);
                let Some(end) = end else {
                    push_lit(&mut s.toks, "[");
                    i += 1;
                    continue;
                };
                let inner: String = chars[i + 1..end].iter().collect();
                let lower = inner.to_ascii_lowercase();
                let color = match lower.as_str() {
                    "red" => Some("#FF0000"),
                    "blue" => Some("#0000FF"),
                    "green" => Some("#00FF00"),
                    "black" => Some("#000000"),
                    "white" => Some("#FFFFFF"),
                    "yellow" => Some("#FFFF00"),
                    "magenta" => Some("#FF00FF"),
                    "cyan" => Some("#00FFFF"),
                    _ => None,
                };
                if color.is_some() {
                    s.color = color;
                } else if let Some(rest) = inner.strip_prefix('$') {
                    // [$€-407]: a currency symbol (and locale); [$-409]: only a locale.
                    let sym = rest.split('-').next().unwrap_or("");
                    if !sym.is_empty() {
                        push_lit(&mut s.toks, sym);
                    }
                } else if !lower.is_empty() && lower.chars().all(|x| x == 'h') {
                    s.toks.push(Tok::ElapsedHour(lower.len()));
                } else if !lower.is_empty() && lower.chars().all(|x| x == 'm') {
                    s.toks.push(Tok::ElapsedMinute(lower.len()));
                } else if !lower.is_empty() && lower.chars().all(|x| x == 's') {
                    s.toks.push(Tok::ElapsedSecond(lower.len()));
                } else if let Some(op) = [">=", "<=", "<>", ">", "<", "="].into_iter().find(|op| inner.starts_with(op)) {
                    s.condition = Some((op.to_owned(), inner[op.len()..].trim().to_owned()));
                }
                i = end + 1;
            }
            '0' | '#' | '?' => {
                let mut j = i;
                let mut text = String::new();
                while j < chars.len() && matches!(chars[j], '0' | '#' | '?' | ',' | '.') {
                    // A `.` after seconds is not part of a number.
                    text.push(chars[j]);
                    j += 1;
                }
                s.toks.push(Tok::Digits(text));
                i = j;
            }
            '.' if matches!(chars.get(i + 1), Some('0')) && s.toks.iter().rev().find(|t| !matches!(t, Tok::Lit(_))).is_some_and(|t| matches!(t, Tok::Second(_) | Tok::ElapsedSecond(_))) => {
                let n = chars[i + 1..].iter().take_while(|&&x| x == '0').count();
                s.toks.push(Tok::SubSecond(n));
                i += 1 + n;
            }
            '.' if matches!(chars.get(i + 1), Some('0' | '#' | '?')) => {
                let mut j = i;
                let mut text = String::new();
                while j < chars.len() && matches!(chars[j], '0' | '#' | '?' | ',' | '.') {
                    text.push(chars[j]);
                    j += 1;
                }
                s.toks.push(Tok::Digits(text));
                i = j;
            }
            '%' => {
                s.toks.push(Tok::Percent);
                i += 1;
            }
            'E' | 'e' if matches!(chars.get(i + 1), Some('+' | '-')) => {
                let n = chars[i + 2..].iter().take_while(|&&x| matches!(x, '0' | '#')).count();
                s.toks.push(Tok::Exp(n as u32));
                i += 2 + n;
            }
            '/' => {
                let prev_digits = matches!(s.toks.last(), Some(Tok::Digits(_)));
                let next_digit = chars.get(i + 1).is_some_and(|x| matches!(x, '0' | '#' | '?') || x.is_ascii_digit());
                if prev_digits && next_digit {
                    s.toks.push(Tok::Slash);
                } else {
                    push_lit(&mut s.toks, "/");
                }
                i += 1;
            }
            'y' | 'Y' => {
                let n = run(&chars, i, 'y');
                s.toks.push(Tok::Year(n));
                i += n;
            }
            'm' | 'M' => {
                let n = run(&chars, i, 'm');
                s.toks.push(Tok::Month(n));
                i += n;
            }
            'd' | 'D' => {
                let n = run(&chars, i, 'd');
                s.toks.push(Tok::Day(n));
                i += n;
            }
            'h' | 'H' => {
                let n = run(&chars, i, 'h');
                s.toks.push(Tok::Hour(n));
                i += n;
            }
            's' | 'S' => {
                let n = run(&chars, i, 's');
                s.toks.push(Tok::Second(n));
                i += n;
            }
            'A' | 'a' => {
                let rest: String = chars[i..chars.len().min(i + 5)].iter().collect::<String>().to_ascii_uppercase();
                if rest.starts_with("AM/PM") {
                    s.toks.push(Tok::AmPm);
                    i += 5;
                } else if rest.starts_with("A/P") {
                    s.toks.push(Tok::AmPm);
                    i += 3;
                } else {
                    push_lit(&mut s.toks, &c.to_string());
                    i += 1;
                }
            }
            'G' | 'g' => {
                let rest: String = chars[i..chars.len().min(i + 7)].iter().collect();
                if rest.eq_ignore_ascii_case("general") {
                    s.toks.push(Tok::General);
                    i += 7;
                } else {
                    push_lit(&mut s.toks, &c.to_string());
                    i += 1;
                }
            }
            '@' => {
                s.toks.push(Tok::At);
                i += 1;
            }
            other => {
                push_lit(&mut s.toks, &other.to_string());
                i += 1;
            }
        }
    }
    // `m` and `mm` are minutes right after hours or right before seconds.
    let date_tok = |t: &Tok| {
        matches!(
            t,
            Tok::Year(_) | Tok::Month(_) | Tok::Day(_) | Tok::Hour(_) | Tok::Second(_) | Tok::ElapsedHour(_) | Tok::ElapsedSecond(_) | Tok::AmPm
        )
    };
    for k in 0..s.toks.len() {
        if let Tok::Month(n) = s.toks[k]
            && n <= 2
        {
            let prev = s.toks[..k].iter().rev().find(|t| date_tok(t));
            let next = s.toks[k + 1..].iter().find(|t| date_tok(t));
            if matches!(prev, Some(Tok::Hour(_) | Tok::ElapsedHour(_))) || matches!(next, Some(Tok::Second(_) | Tok::ElapsedSecond(_))) {
                s.toks[k] = Tok::Minute(n);
            }
        }
    }
    s
}

fn text_el(t: &str, out: &mut String) {
    if !t.is_empty() {
        out.push_str("<number:text>");
        out.push_str(&escape(t));
        out.push_str("</number:text>");
    }
}

/// A numeric section's body (number, scientific or fraction) from its digit tokens.
fn number_body(toks: &[Tok], out: &mut String) {
    // The pattern from the first digit run to the last, with the text between kept as embedded text.
    let first = toks.iter().position(|t| matches!(t, Tok::Digits(_)));
    let last = toks.iter().rposition(|t| matches!(t, Tok::Digits(_)));
    let (Some(first), Some(last)) = (first, last) else { return };
    let body = &toks[first..=last];
    let exp = toks.iter().find_map(|t| if let Tok::Exp(n) = t { Some(*n) } else { None });
    let digits: String = body.iter().filter_map(|t| if let Tok::Digits(d) = t { Some(d.as_str()) } else { None }).collect();
    if let Some(slash) = toks.iter().position(|t| *t == Tok::Slash) {
        // `# ?/?`, `?/?`, `# ??/16`.
        let before = &toks[first..slash];
        let numerator = before.iter().rev().find_map(|t| if let Tok::Digits(d) = t { Some(d.len()) } else { None }).unwrap_or(1);
        let whole = before.iter().filter(|t| matches!(t, Tok::Digits(_))).count() > 1;
        let after = &toks[slash + 1..];
        let den_digits = after.iter().take_while(|t| !matches!(t, Tok::Lit(l) if l.trim().parse::<u32>().is_err())).find_map(|t| {
            if let Tok::Digits(d) = t { Some(d.len()) } else { None }
        });
        let den_value = match after.first() {
            Some(Tok::Lit(l)) => l.trim().parse::<u32>().ok(),
            _ => None,
        };
        out.push_str("<number:fraction");
        if whole {
            let w = before.iter().find_map(|t| if let Tok::Digits(d) = t { Some(d) } else { None }).map_or(0, |d| d.matches('0').count());
            out.push_str(&format!(" number:min-integer-digits=\"{w}\""));
        }
        out.push_str(&format!(" number:min-numerator-digits=\"{numerator}\""));
        match den_value {
            Some(v) => out.push_str(&format!(" number:min-denominator-digits=\"{}\" number:denominator-value=\"{v}\"", v.to_string().len())),
            None => out.push_str(&format!(" number:min-denominator-digits=\"{}\"", den_digits.unwrap_or(1))),
        }
        out.push_str("/>");
        return;
    }
    let (int_part, frac_part) = match digits.split_once('.') {
        Some((a, b)) => (a.to_owned(), b.to_owned()),
        None => (digits.clone(), String::new()),
    };
    let frac_placeholders: String = frac_part.chars().filter(|c| matches!(c, '0' | '#' | '?')).collect();
    let decimals = frac_placeholders.len();
    let min_decimals = frac_placeholders.trim_end_matches(['#', '?']).len();
    if let Some(e) = exp {
        let w = int_part.chars().filter(|c| matches!(c, '0' | '#' | '?')).count();
        let zeros = int_part.matches('0').count();
        out.push_str(&format!(
            "<number:scientific-number number:decimal-places=\"{decimals}\" number:min-integer-digits=\"{}\" number:min-exponent-digits=\"{}\"",
            zeros.max(1),
            e.max(1)
        ));
        if w >= 2 && int_part.contains('#') {
            out.push_str(&format!(" number:exponent-interval=\"{w}\""));
        }
        out.push_str("/>");
        return;
    }
    // Thousands scaling: commas after the last digit placeholder.
    let all = format!("{int_part}{}{frac_part}", if frac_part.is_empty() && !digits.contains('.') { "" } else { "." });
    let trailing_commas = all.chars().rev().take_while(|&c| c == ',').count();
    let int_core = int_part.trim_end_matches(',');
    let grouping = int_core.contains(',');
    let min_int = int_core.matches('0').count();
    let factor = 1000f64.powi(if frac_part.is_empty() { trailing_commas as i32 } else { frac_part.chars().rev().take_while(|&c| c == ',').count() as i32 });
    out.push_str(&format!("<number:number number:decimal-places=\"{decimals}\""));
    if min_decimals != decimals {
        out.push_str(&format!(" number:min-decimal-places=\"{min_decimals}\""));
    } else {
        out.push_str(&format!(" number:min-decimal-places=\"{decimals}\""));
    }
    out.push_str(&format!(" number:min-integer-digits=\"{min_int}\""));
    if grouping {
        out.push_str(" number:grouping=\"true\"");
    }
    if factor > 1.0 {
        out.push_str(&format!(" number:display-factor=\"{factor}\""));
    }
    // Text between the integer digits.
    let mut embedded: Vec<(usize, String)> = Vec::new();
    let mut digits_right = 0usize;
    let int_toks: Vec<&Tok> = body.iter().take_while(|t| !matches!(t, Tok::Digits(d) if d.contains('.'))).collect();
    for t in int_toks.iter().rev() {
        match t {
            Tok::Digits(d) => digits_right += d.chars().filter(|c| matches!(c, '0' | '#' | '?')).count(),
            Tok::Lit(l) if digits_right > 0 => embedded.push((digits_right, l.clone())),
            _ => {}
        }
    }
    if embedded.is_empty() {
        out.push_str("/>");
    } else {
        out.push('>');
        for (pos, text) in embedded.iter().rev() {
            out.push_str(&format!("<number:embedded-text number:position=\"{pos}\">{}</number:embedded-text>", escape(text)));
        }
        out.push_str("</number:number>");
    }
}

/// The ODF children of one section, and the kind of style it needs.
fn section_xml(sec: &Section) -> (StyleKind, String) {
    let toks = &sec.toks;
    let has = |f: &dyn Fn(&Tok) -> bool| toks.iter().any(f);
    let date = has(&|t| matches!(t, Tok::Year(_) | Tok::Month(_) | Tok::Day(_)));
    let time = has(&|t| {
        matches!(
            t,
            Tok::Hour(_) | Tok::Minute(_) | Tok::Second(_) | Tok::ElapsedHour(_) | Tok::ElapsedMinute(_) | Tok::ElapsedSecond(_) | Tok::AmPm
        )
    });
    let kind = if date {
        StyleKind::Date
    } else if time {
        StyleKind::Time
    } else if has(&|t| *t == Tok::At) {
        StyleKind::Text
    } else if has(&|t| *t == Tok::Percent) {
        StyleKind::Percentage
    } else {
        StyleKind::Number
    };
    let mut out = String::new();
    if let Some(c) = sec.color {
        out.push_str(&format!("<style:text-properties fo:color=\"{c}\"/>"));
    }
    match kind {
        StyleKind::Date | StyleKind::Time => {
            let mut k = 0;
            while k < toks.len() {
                match &toks[k] {
                    Tok::Lit(l) => text_el(l, &mut out),
                    Tok::Year(n) => out.push_str(if *n > 2 { "<number:year number:style=\"long\"/>" } else { "<number:year/>" }),
                    Tok::Month(n) => out.push_str(match n {
                        1 => "<number:month/>",
                        2 => "<number:month number:style=\"long\"/>",
                        3 | 5 => "<number:month number:textual=\"true\"/>",
                        _ => "<number:month number:style=\"long\" number:textual=\"true\"/>",
                    }),
                    Tok::Day(n) => out.push_str(match n {
                        1 => "<number:day/>",
                        2 => "<number:day number:style=\"long\"/>",
                        3 => "<number:day-of-week/>",
                        _ => "<number:day-of-week number:style=\"long\"/>",
                    }),
                    Tok::Hour(n) | Tok::ElapsedHour(n) => {
                        out.push_str(if *n > 1 { "<number:hours number:style=\"long\"/>" } else { "<number:hours/>" })
                    }
                    Tok::Minute(n) | Tok::ElapsedMinute(n) => {
                        out.push_str(if *n > 1 { "<number:minutes number:style=\"long\"/>" } else { "<number:minutes/>" })
                    }
                    Tok::Second(n) | Tok::ElapsedSecond(n) => {
                        let decimals = match toks.get(k + 1) {
                            Some(Tok::SubSecond(d)) => {
                                k += 1;
                                *d
                            }
                            _ => 0,
                        };
                        out.push_str("<number:seconds");
                        if *n > 1 {
                            out.push_str(" number:style=\"long\"");
                        }
                        if decimals > 0 {
                            out.push_str(&format!(" number:decimal-places=\"{decimals}\""));
                        }
                        out.push_str("/>");
                    }
                    Tok::AmPm => out.push_str("<number:am-pm/>"),
                    Tok::Digits(d) => text_el(d, &mut out),
                    Tok::Percent => text_el("%", &mut out),
                    Tok::Slash => text_el("/", &mut out),
                    _ => {}
                }
                k += 1;
            }
        }
        StyleKind::Text => {
            for t in toks {
                match t {
                    Tok::Lit(l) => text_el(l, &mut out),
                    Tok::At => out.push_str("<number:text-content/>"),
                    _ => {}
                }
            }
        }
        _ => {
            if toks.iter().any(|t| *t == Tok::General) && !toks.iter().any(|t| matches!(t, Tok::Digits(_))) {
                for t in toks {
                    match t {
                        Tok::Lit(l) => text_el(l, &mut out),
                        Tok::General => out.push_str("<number:number number:min-integer-digits=\"1\"/>"),
                        Tok::Percent => text_el("%", &mut out),
                        _ => {}
                    }
                }
            } else {
                let first = toks.iter().position(|t| matches!(t, Tok::Digits(_)));
                let last = toks.iter().rposition(|t| matches!(t, Tok::Digits(_) | Tok::Exp(_)));
                match (first, last) {
                    (Some(f), Some(l)) => {
                        for t in &toks[..f] {
                            match t {
                                Tok::Lit(s) => text_el(s, &mut out),
                                Tok::Percent => text_el("%", &mut out),
                                _ => {}
                            }
                        }
                        number_body(toks, &mut out);
                        let is_fraction = toks.contains(&Tok::Slash);
                        for t in &toks[l + 1..] {
                            match t {
                                // A fixed denominator was written into the fraction.
                                Tok::Lit(s) if is_fraction && s.trim().parse::<u32>().is_ok() => {}
                                Tok::Lit(s) => text_el(s, &mut out),
                                Tok::Percent => text_el("%", &mut out),
                                _ => {}
                            }
                        }
                    }
                    _ => {
                        for t in toks {
                            match t {
                                Tok::Lit(s) => text_el(s, &mut out),
                                Tok::Percent => text_el("%", &mut out),
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
    }
    (kind, out)
}

/// What kind of value a format shows, for choosing the value type cells are written with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shows {
    Number,
    Percent,
    /// A date (with or without a time).
    Date,
    /// A time of day or a duration.
    Time,
    Text,
}

/// The data style XML for an Excel code (sections after the first become styles named `{name}P0`, `{name}P1`),
/// and what the code shows. None for General.
pub(crate) fn odf_style(name: &str, code: &str) -> Option<(String, Shows)> {
    let code = code.trim();
    if code.is_empty() || code.eq_ignore_ascii_case("general") {
        return None;
    }
    let mut sections: Vec<Section> = split_sections(code).iter().map(|s| tokenize(s)).collect();
    // A text section (`…;@`) can't follow numeric ones in ODF; the numeric ones are kept.
    if sections.len() > 1 && sections.last().is_some_and(|s| s.toks.contains(&Tok::At)) {
        sections.pop();
    }
    sections.truncate(3);
    let shows_of = |k: StyleKind| match k {
        StyleKind::Date => Shows::Date,
        StyleKind::Time => Shows::Time,
        StyleKind::Percentage => Shows::Percent,
        StyleKind::Text => Shows::Text,
        _ => Shows::Number,
    };
    let rendered: Vec<(StyleKind, String)> = sections.iter().map(section_xml).collect();
    let main_kind = rendered.first().map(|r| r.0).unwrap_or(StyleKind::Number);
    let mut out = String::new();
    let elapsed = |s: &Section| s.toks.iter().any(|t| matches!(t, Tok::ElapsedHour(_) | Tok::ElapsedMinute(_) | Tok::ElapsedSecond(_)));
    let open = |out: &mut String, kind: StyleKind, n: &str, sec: &Section| {
        out.push('<');
        out.push_str(kind.element());
        out.push_str(&format!(" style:name=\"{}\"", escape(n)));
        if elapsed(sec) {
            out.push_str(" number:truncate-on-overflow=\"false\"");
        }
        out.push('>');
    };
    let close = |out: &mut String, kind: StyleKind| {
        out.push_str("</");
        out.push_str(kind.element());
        out.push('>');
    };
    let cond_text = |c: &(String, String)| {
        let op = match c.0.as_str() {
            "<>" => "!=",
            "=" => "=",
            o => o,
        };
        format!("value(){op}{}", c.1)
    };
    if rendered.len() == 1 {
        let (kind, body) = &rendered[0];
        open(&mut out, *kind, name, &sections[0]);
        out.push_str(body);
        close(&mut out, *kind);
        return Some((out, shows_of(*kind)));
    }
    // Every section but the last is its own style; the last is the main style and maps to the others.
    let n = rendered.len();
    let mut maps = String::new();
    for i in 0..n - 1 {
        let sub = format!("{name}P{i}");
        let (kind, body) = &rendered[i];
        // All sections of a style share its kind (ODF can't map a number style to a date style).
        let kind = if matches!(kind, StyleKind::Date | StyleKind::Time) == matches!(main_kind, StyleKind::Date | StyleKind::Time) { *kind } else { main_kind };
        open(&mut out, kind, &sub, &sections[i]);
        out.push_str(body);
        close(&mut out, kind);
        let condition = match &sections[i].condition {
            Some(c) => cond_text(c),
            None if n == 2 => "value()>=0".to_owned(),
            None if i == 0 => "value()>0".to_owned(),
            None => "value()<0".to_owned(),
        };
        maps.push_str(&format!("<style:map style:condition=\"{}\" style:apply-style-name=\"{}\"/>", escape(&condition), escape(&sub)));
    }
    let (kind, body) = &rendered[n - 1];
    let kind = if matches!(kind, StyleKind::Text) { main_kind } else { *kind };
    open(&mut out, kind, name, &sections[n - 1]);
    out.push_str(body);
    out.push_str(&maps);
    close(&mut out, kind);
    Some((out, shows_of(main_kind)))
}

/// Reads every data style in an XML fragment (for tests and for parts holding only styles).
#[cfg(test)]
fn read_all(xml: &str) -> HashMap<String, DataStyle> {
    let mut r = crate::formats::xml::reader(xml.as_bytes());
    let mut buf = Vec::new();
    let mut out = HashMap::default();
    loop {
        buf.clear();
        let ev = match r.read_event_into(&mut buf) {
            Ok(ev) => ev,
            Err(_) => break,
        };
        match &ev {
            Event::Start(e) | Event::Empty(e) => {
                if let Some(kind) = StyleKind::of_element(local(e.name().into_inner())) {
                    let e = e.to_owned();
                    let (name, ds) = read(&mut r, &e, kind, matches!(ev, Event::Empty(_)));
                    out.insert(name, ds);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap(inner: &str) -> String {
        format!(
            "<office:automatic-styles xmlns:office=\"o\" xmlns:number=\"n\" xmlns:style=\"s\" xmlns:fo=\"f\">{inner}</office:automatic-styles>"
        )
    }

    /// Excel code → ODF → Excel code.
    fn round(code: &str) -> String {
        let (xml, _) = odf_style("N1", code).unwrap_or_else(|| panic!("{code} has a style"));
        let styles = read_all(&wrap(&xml));
        excel_code("N1", &styles).unwrap_or_else(|| panic!("{code}: {xml}"))
    }

    #[test]
    fn libreoffice_styles() {
        let xml = wrap(
            r##"<number:number-style style:name="N104P0" style:volatile="true"><number:number number:decimal-places="2" number:min-decimal-places="2" number:min-integer-digits="1" number:grouping="true"/></number:number-style>
<number:number-style style:name="N104"><style:text-properties fo:color="#ff0000"/><number:text>-</number:text><number:number number:decimal-places="2" number:min-decimal-places="2" number:min-integer-digits="1" number:grouping="true"/><style:map style:condition="value()&gt;=0" style:apply-style-name="N104P0"/></number:number-style>
<number:date-style style:name="N37"><number:day number:style="long"/><number:text>.</number:text><number:month number:style="long"/><number:text>.</number:text><number:year number:style="long"/></number:date-style>
<number:time-style style:name="N46" number:truncate-on-overflow="false"><number:hours/><number:text>:</number:text><number:minutes number:style="long"/><number:text>:</number:text><number:seconds number:style="long"/></number:time-style>
<number:percentage-style style:name="N11"><number:number number:decimal-places="1" number:min-decimal-places="1" number:min-integer-digits="1"/><number:text>%</number:text></number:percentage-style>
<number:currency-style style:name="N120P0" style:volatile="true"><number:currency-symbol number:language="en" number:country="US">$</number:currency-symbol><number:number number:decimal-places="2" number:min-decimal-places="2" number:min-integer-digits="1" number:grouping="true"/></number:currency-style>
<number:currency-style style:name="N120"><style:text-properties fo:color="#ff0000"/><number:text>-</number:text><number:currency-symbol number:language="en" number:country="US">$</number:currency-symbol><number:number number:decimal-places="2" number:min-decimal-places="2" number:min-integer-digits="1" number:grouping="true"/><style:map style:condition="value()&gt;=0" style:apply-style-name="N120P0"/></number:currency-style>
<number:date-style style:name="N50"><number:month number:textual="true"/><number:text> </number:text><number:day/><number:text>, </number:text><number:year number:style="long"/><number:text> </number:text><number:hours number:style="long"/><number:text>:</number:text><number:minutes number:style="long"/><number:text> </number:text><number:am-pm/></number:date-style>
<number:number-style style:name="N60"><number:scientific-number number:decimal-places="2" number:min-integer-digits="1" number:min-exponent-digits="2"/></number:number-style>
<number:number-style style:name="N61"><number:fraction number:min-integer-digits="0" number:min-numerator-digits="1" number:min-denominator-digits="1"/></number:number-style>
<number:text-style style:name="N100"><number:text-content/></number:text-style>
<number:boolean-style style:name="N99"><number:boolean/></number:boolean-style>
<number:number-style style:name="N0"><number:number number:min-integer-digits="1"/></number:number-style>
<number:number-style style:name="N70"><number:number number:decimal-places="0" number:min-integer-digits="9"><number:embedded-text number:position="4">-</number:embedded-text><number:embedded-text number:position="6">-</number:embedded-text></number:number></number:number-style>"##,
        );
        let s = read_all(&xml);
        let code = |n: &str| excel_code(n, &s);
        assert_eq!(code("N104").as_deref(), Some("#,##0.00;[Red]-#,##0.00"));
        assert_eq!(code("N37").as_deref(), Some("dd.mm.yyyy"));
        assert_eq!(code("N46").as_deref(), Some("[h]:mm:ss"));
        assert_eq!(code("N11").as_deref(), Some("0.0%"));
        assert_eq!(code("N120").as_deref(), Some("\"$\"#,##0.00;[Red]-\"$\"#,##0.00"));
        assert_eq!(code("N50").as_deref(), Some("mmm d, yyyy hh:mm AM/PM"));
        assert_eq!(code("N60").as_deref(), Some("0.00E+00"));
        assert_eq!(code("N61").as_deref(), Some("# ?/?"));
        assert_eq!(code("N100").as_deref(), Some("@"));
        assert_eq!(code("N99"), None);
        assert_eq!(code("N0"), None);
        assert_eq!(code("N70").as_deref(), Some("000-00-0000"));
        // What the engine shows with them.
        let shown = |n: &str, v: f64| crate::numfmt::format_value(&crate::value::Value::Number(v), code(n).as_deref()).0;
        assert_eq!(shown("N104", -1234.5), "-1,234.50");
        assert_eq!(shown("N37", 46312.0), "17.10.2026");
        assert_eq!(shown("N11", 0.125), "12.5%");
    }

    #[test]
    fn excel_codes_round_trip() {
        for code in [
            "0",
            "0.00",
            "#,##0",
            "#,##0.00",
            "0%",
            "0.00%",
            "0.00E+00",
            "##0.0E+0",
            "yyyy-mm-dd",
            "m/d/yyyy",
            "d-mmm-yy",
            "mmm-yy",
            "mmmm d, yyyy",
            "dddd",
            "h:mm AM/PM",
            "h:mm:ss",
            "hh:mm:ss",
            "[h]:mm:ss",
            "mm:ss",
            "m/d/yyyy h:mm",
            "yyyy-mm-dd hh:mm:ss",
            "h:mm:ss.00",
            "@",
            "#,##0.00;[Red]-#,##0.00",
            "\"$\"#,##0.00",
            "0.00;(0.00);\"-\"",
            "[>=1000]#,##0;0",
            "# ?/?",
            "# ??/16",
            "000-00-0000",
            "#,##0,",
            "0.0#",
            "\"Total: \"0",
        ] {
            assert_eq!(round(code), code, "{code}");
        }
        // Codes ODF can't hold exactly come back showing the same thing.
        assert_eq!(round("$#,##0"), "\"$\"#,##0");
        assert_eq!(round("[$€-407]#,##0.00"), "\"€\"#,##0.00");
        assert_eq!(round("[$-409]m/d/yyyy"), "m/d/yyyy");
        assert_eq!(round("0.00;-0.00;0;@"), "0.00;-0.00;0");
        assert_eq!(odf_style("N1", "General"), None);
        assert_eq!(odf_style("N1", "yyyy-mm-dd").map(|s| s.1), Some(Shows::Date));
        assert_eq!(odf_style("N1", "h:mm").map(|s| s.1), Some(Shows::Time));
        assert_eq!(odf_style("N1", "0%").map(|s| s.1), Some(Shows::Percent));
        assert_eq!(odf_style("N1", "@").map(|s| s.1), Some(Shows::Text));
    }
}
