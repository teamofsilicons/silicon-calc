//! Readable output: aligned tables, a grid of cells with column letters and row numbers, times.

use serde_json::Value as Json;
use silicon_calc_client::a1::col_name;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// The widest a table column grows before its text is cut with `…`.
pub const MAX_COLUMN_WIDTH: usize = 60;

/// Keeps a cell on one line: newlines and tabs become `\n` and `\t`.
pub fn one_line(s: &str) -> String {
    if !s.contains(['\n', '\r', '\t']) {
        return s.to_owned();
    }
    s.replace("\r\n", "\\n").replace('\n', "\\n").replace('\r', "\\n").replace('\t', "\\t")
}

/// Cuts `s` to at most `width` columns, ending with `…` when cut.
pub fn truncate(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if w + cw + 1 > width {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

fn pad(s: &str, width: usize, right: bool) -> String {
    let fill = width.saturating_sub(s.width());
    if right { format!("{}{s}", " ".repeat(fill)) } else { format!("{s}{}", " ".repeat(fill)) }
}

/// A table with a header row, columns two spaces apart.
pub struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    right: Vec<bool>,
    max_width: usize,
}

impl Table {
    pub fn new(headers: &[&str]) -> Self {
        Self {
            headers: headers.iter().map(|h| h.to_uppercase()).collect(),
            rows: Vec::new(),
            right: vec![false; headers.len()],
            max_width: MAX_COLUMN_WIDTH,
        }
    }

    /// A table whose headers are shown exactly as written (column names of data).
    pub fn raw(headers: &[String]) -> Self {
        Self {
            headers: headers.iter().map(|h| truncate(&one_line(h), MAX_COLUMN_WIDTH)).collect(),
            rows: Vec::new(),
            right: vec![false; headers.len()],
            max_width: MAX_COLUMN_WIDTH,
        }
    }

    /// Right-aligns a column (numbers).
    pub fn right(mut self, col: usize) -> Self {
        if col < self.right.len() {
            self.right[col] = true;
        }
        self
    }

    pub fn max_width(mut self, width: usize) -> Self {
        self.max_width = width.max(4);
        self
    }

    pub fn row(&mut self, cells: Vec<String>) {
        self.rows.push(cells.into_iter().map(|c| truncate(&one_line(&c), self.max_width)).collect());
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn render(&self) -> String {
        let cols = self.headers.len();
        let mut widths: Vec<usize> = self.headers.iter().map(|h| h.width()).collect();
        for row in &self.rows {
            for (i, cell) in row.iter().enumerate().take(cols) {
                widths[i] = widths[i].max(cell.width());
            }
        }
        let mut out = String::new();
        let line = |cells: &[String], out: &mut String| {
            let mut parts = Vec::with_capacity(cols);
            for i in 0..cols {
                let cell = cells.get(i).map(String::as_str).unwrap_or("");
                // The last column isn't padded, so lines don't end in spaces.
                if i + 1 == cols && !self.right[i] {
                    parts.push(cell.to_owned());
                } else {
                    parts.push(pad(cell, widths[i], self.right[i]));
                }
            }
            out.push_str(parts.join("  ").trim_end());
            out.push('\n');
        };
        line(&self.headers, &mut out);
        for row in &self.rows {
            line(row, &mut out);
        }
        out
    }
}

/// One cell of a grid.
pub struct GridCell {
    pub row: u32,
    pub col: u32,
    pub text: String,
    /// Right-aligned (numbers).
    pub right: bool,
}

/// A block of cells as a spreadsheet shows it: column letters across, row numbers down.
pub fn grid(cells: &[GridCell], r1: u32, c1: u32, r2: u32, c2: u32, max_width: usize) -> String {
    let rows = (r2 - r1 + 1) as usize;
    let cols = (c2 - c1 + 1) as usize;
    let mut texts: Vec<Vec<(String, bool)>> = vec![vec![(String::new(), false); cols]; rows];
    for c in cells {
        if c.row < r1 || c.row > r2 || c.col < c1 || c.col > c2 {
            continue;
        }
        texts[(c.row - r1) as usize][(c.col - c1) as usize] = (truncate(&one_line(&c.text), max_width), c.right);
    }
    let headers: Vec<String> = (c1..=c2).map(col_name).collect();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.width()).collect();
    for row in &texts {
        for (i, (t, _)) in row.iter().enumerate() {
            widths[i] = widths[i].max(t.width());
        }
    }
    let label_width = (r2 + 1).to_string().len();
    let mut out = String::new();
    let mut head = vec![" ".repeat(label_width)];
    head.extend(headers.iter().enumerate().map(|(i, h)| pad(h, widths[i], false)));
    out.push_str(head.join("  ").trim_end());
    out.push('\n');
    for (ri, row) in texts.iter().enumerate() {
        let mut parts = vec![pad(&(r1 as usize + ri + 1).to_string(), label_width, true)];
        parts.extend(row.iter().enumerate().map(|(i, (t, right))| pad(t, widths[i], *right)));
        out.push_str(parts.join("  ").trim_end());
        out.push('\n');
    }
    out
}

pub fn parse_time(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s.trim(), &Rfc3339).ok()
}

/// `2026-10-17 08:12 UTC`.
pub fn when(s: &str) -> String {
    match parse_time(s) {
        Some(t) => {
            let t = t.to_offset(time::UtcOffset::UTC);
            format!("{:04}-{:02}-{:02} {:02}:{:02} UTC", t.year(), u8::from(t.month()), t.day(), t.hour(), t.minute())
        }
        None => s.to_owned(),
    }
}

/// `just now`, `5 min ago`, `3 h ago`, `2 days ago`, then the date.
pub fn ago(s: &str) -> String {
    let Some(t) = parse_time(s) else { return s.to_owned() };
    let secs = (OffsetDateTime::now_utc() - t).whole_seconds();
    match secs {
        i64::MIN..=-60 => when(s),
        -59..=44 => "just now".into(),
        45..=3599 => format!("{} min ago", (secs + 30) / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        86_400..=172_799 => "yesterday".into(),
        172_800..=604_799 => format!("{} days ago", secs / 86_400),
        _ => {
            let t = t.to_offset(time::UtcOffset::UTC);
            format!("{:04}-{:02}-{:02}", t.year(), u8::from(t.month()), t.day())
        }
    }
}

/// `2026-10-17 08:12 UTC (5 min ago)`.
pub fn when_ago(s: &str) -> String {
    if parse_time(s).is_none() {
        return s.to_owned();
    }
    format!("{} ({})", when(s), ago(s))
}

/// A plain JSON value as a cell shows it.
pub fn json_text(v: &Json) -> String {
    match v {
        Json::Null => String::new(),
        Json::Bool(true) => "TRUE".into(),
        Json::Bool(false) => "FALSE".into(),
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                return i.to_string();
            }
            if let Some(u) = n.as_u64() {
                return u.to_string();
            }
            match n.as_f64() {
                Some(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", f as i64),
                Some(f) => format!("{f}"),
                None => n.to_string(),
            }
        }
        Json::String(s) => s.clone(),
        Json::Object(m) if m.contains_key("t") => match (m.get("t").and_then(Json::as_str), m.get("v")) {
            (Some("empty"), _) | (_, None) => String::new(),
            (_, Some(v)) => json_text(v),
        },
        other => other.to_string(),
    }
}

/// True when a JSON value is a number (for right alignment).
pub fn json_is_number(v: &Json) -> bool {
    matches!(v, Json::Number(_)) || v.get("t").and_then(Json::as_str) == Some("number")
}

/// `1 cell`, `3 cells`.
pub fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{} {}", thousands(n), if n == 1 { one } else { many })
}

/// `1,234,567`.
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_align() {
        let mut t = Table::new(&["id", "title", "rows"]).right(2);
        t.row(vec!["k3j9xq2mpa7d".into(), "Q3 revenue".into(), "120".into()]);
        t.row(vec!["a".into(), "Café ☕".into(), "7".into()]);
        let out = t.render();
        assert_eq!(out, "ID            TITLE       ROWS\nk3j9xq2mpa7d  Q3 revenue   120\na             Café ☕         7\n");
    }

    #[test]
    fn tables_keep_rows_on_one_line() {
        let mut t = Table::new(&["a", "b"]).max_width(8);
        t.row(vec!["line1\nline2".into(), "a very long text".into()]);
        let out = t.render();
        assert_eq!(out.lines().count(), 2);
        assert!(out.contains("line1\\n…"), "{out}");
        assert!(out.contains("a very …"), "{out}");
    }

    #[test]
    fn grids_show_letters_and_numbers() {
        let cells = vec![
            GridCell { row: 0, col: 0, text: "region".into(), right: false },
            GridCell { row: 0, col: 1, text: "amount".into(), right: false },
            GridCell { row: 1, col: 0, text: "EU".into(), right: false },
            GridCell { row: 1, col: 1, text: "1,200.50".into(), right: true },
            GridCell { row: 9, col: 1, text: "x".into(), right: false },
        ];
        let out = grid(&cells, 0, 0, 2, 1, 40);
        assert_eq!(out, "   A       B\n1  region  amount\n2  EU      1,200.50\n3\n");
    }

    #[test]
    fn numbers_and_times() {
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(thousands(12), "12");
        assert_eq!(plural(1, "cell", "cells"), "1 cell");
        assert_eq!(plural(2000, "cell", "cells"), "2,000 cells");
        assert_eq!(json_text(&serde_json::json!(3.0)), "3");
        assert_eq!(json_text(&serde_json::json!(2.5)), "2.5");
        assert_eq!(json_text(&serde_json::json!(null)), "");
        assert_eq!(json_text(&serde_json::json!({"t": "number", "v": 4})), "4");
        assert_eq!(when("2026-10-17T08:12:30Z"), "2026-10-17 08:12 UTC");
        assert_eq!(ago("not a time"), "not a time");
        let now = OffsetDateTime::now_utc().format(&Rfc3339).unwrap();
        assert_eq!(ago(&now), "just now");
    }
}
