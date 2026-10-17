//! JSON.
//!
//! Writing gives `{"tabs": [{"name": "Sales", "rows": [[…], …]}]}`: every tab (or the one asked for) as rows of
//! what its cells hold, one row per line. Numbers are JSON numbers (15 significant digits, as a cell shows them),
//! dates and times are ISO 8601 text (`2026-10-17`, `2026-10-17T14:30:00`, `14:30:00`), errors are their text,
//! empty cells are `null`. Unless [`ExportOptions::values_only`] is set, a cell holding a formula is
//! `{"v": its value, "f": "=SUM(B2:B9)"}`.
//!
//! Reading takes the shapes data comes in, without ever holding the whole file as a tree (so key order is kept
//! and a million rows fit):
//! - that same `{"tabs": […]}` shape, formulas included;
//! - an array of objects: the keys, in the order they first appear, become the header row;
//! - an array of arrays (a plain grid), or of single values (one column);
//! - an object whose values are arrays or objects: one tab per key; when every value is a list of single values
//!   (`{"name": [...], "age": [...]}`) it is one table with the keys as the header row;
//! - JSON Lines (`.jsonl`): one object or array per line.
//!
//! Strings stay text, except ISO 8601 dates and times (which become dates) and error values like `#N/A`. Whole
//! numbers longer than 15 digits stay text so no digit is lost; nested arrays and objects inside a cell are kept
//! as their JSON text.

use std::{fmt, io::Write as _};

use ahash::HashMap;
use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};

use crate::{
    addr::CellPos,
    dates::{hms_from_serial, ymd_from_serial},
    formats::{
        ExportOptions, FormatError, ImportOptions, Imported, export_tabs,
        numfmts::{DateKind, date_kind},
        number_text,
        text::decode_text,
    },
    model::Tab,
    ops::{ColumnContent, TabContent},
    style::Style,
    value::{ErrorKind, Value},
    workbook::Workbook,
};

const FORMAT: &str = "JSON";

// ---- reading ----------------------------------------------------------------------------------------------------

/// A cell as it came out of the file.
struct CellIn {
    value: Value,
    /// The number format a date or time gets.
    num_fmt: Option<&'static str>,
    /// Formula text without `=`.
    formula: Option<String>,
}

impl CellIn {
    fn plain(value: Value) -> Self {
        Self { value, num_fmt: None, formula: None }
    }
}

/// One tab being built.
struct TabBuilder {
    name: String,
    columns: Vec<ColumnContent>,
    styles: Vec<Style>,
    style_of: HashMap<&'static str, u32>,
    rows: u32,
    /// Object rows: column of each key, and whether row 0 is the header row they fill.
    keys: HashMap<String, u32>,
    has_header: bool,
    /// What kinds of rows it was made of (to tell a column of values from a table).
    saw_scalar: bool,
    saw_compound: bool,
}

impl TabBuilder {
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            columns: Vec::new(),
            styles: Vec::new(),
            style_of: HashMap::default(),
            rows: 0,
            keys: HashMap::default(),
            has_header: false,
            saw_scalar: false,
            saw_compound: false,
        }
    }

    fn set(&mut self, row: u32, col: u32, cell: CellIn) -> Result<(), String> {
        if col >= crate::addr::MAX_COLS {
            return Err("a row has more than 16,384 values".to_owned());
        }
        if row >= crate::addr::MAX_ROWS {
            return Err(format!("it has more than {} rows", crate::addr::MAX_ROWS));
        }
        if self.columns.len() <= col as usize {
            self.columns.resize_with(col as usize + 1, ColumnContent::default);
        }
        let style = cell.num_fmt.map(|f| {
            let next = self.styles.len() as u32;
            *self.style_of.entry(f).or_insert_with(|| {
                self.styles.push(Style { num_fmt: Some(f.to_owned()), ..Default::default() });
                next
            })
        });
        let column = &mut self.columns[col as usize];
        if let Some(ix) = style {
            column.styles.push((row, ix));
        }
        if let Some(f) = cell.formula {
            column.formulas.push((row, f));
        }
        if !cell.value.is_empty() || column.formulas.last().is_some_and(|f| f.0 == row) {
            let len = column.values.len() as u32;
            if len < row {
                column.values.resize(row as usize, Value::Empty);
            }
            if (column.values.len() as u32) == row {
                column.values.push(cell.value);
            } else {
                column.values[row as usize] = cell.value;
            }
        }
        Ok(())
    }

    /// The column of an object key, adding it to the header row when it is new.
    fn key_col(&mut self, key: &str) -> Result<u32, String> {
        if let Some(&c) = self.keys.get(key) {
            return Ok(c);
        }
        if !self.has_header {
            // Row 0 holds the header; the first object row comes after it.
            self.has_header = true;
            if self.rows == 0 {
                self.rows = 1;
            }
        }
        let c = self.keys.len() as u32;
        self.keys.insert(key.to_owned(), c);
        self.set(0, c, CellIn::plain(Value::from(key)))?;
        Ok(c)
    }

    fn finish(mut self) -> (String, TabContent) {
        for col in self.columns.iter_mut() {
            while col.values.last().is_some_and(Value::is_empty) && col.formulas.last().is_none_or(|f| (f.0 as usize) < col.values.len() - 1) {
                col.values.pop();
            }
            col.styles.sort_by_key(|s| s.0);
            col.formulas.sort_by_key(|f| f.0);
        }
        let cols = self.columns.len() as u32;
        let content = TabContent { rows: self.rows, cols, columns: self.columns, styles: self.styles, ..Default::default() };
        (self.name, content)
    }
}

/// A value nested inside a cell, kept in order so it can be written back as the JSON text it was.
enum Node {
    Null,
    Bool(bool),
    Number(String),
    Str(String),
    Arr(Vec<Node>),
    Obj(Vec<(String, Node)>),
}

impl Node {
    fn write_json(&self, out: &mut String) {
        match self {
            Node::Null => out.push_str("null"),
            Node::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Node::Number(n) => out.push_str(n),
            Node::Str(s) => push_json_str(out, s),
            Node::Arr(items) => {
                out.push('[');
                for (i, n) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    n.write_json(out);
                }
                out.push(']');
            }
            Node::Obj(entries) => {
                out.push('{');
                for (i, (k, n)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    push_json_str(out, k);
                    out.push(':');
                    n.write_json(out);
                }
                out.push('}');
            }
        }
    }

    fn json_text(&self) -> String {
        let mut s = String::new();
        self.write_json(&mut s);
        s
    }
}

fn push_json_str(out: &mut String, s: &str) {
    out.push_str(&serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_owned()));
}

struct NodeSeed;

impl<'de> DeserializeSeed<'de> for NodeSeed {
    type Value = Node;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Node, D::Error> {
        d.deserialize_any(NodeVisitor)
    }
}

struct NodeVisitor;

impl<'de> Visitor<'de> for NodeVisitor {
    type Value = Node;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_unit<E>(self) -> Result<Node, E> {
        Ok(Node::Null)
    }

    fn visit_none<E>(self) -> Result<Node, E> {
        Ok(Node::Null)
    }

    fn visit_bool<E>(self, v: bool) -> Result<Node, E> {
        Ok(Node::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Node, E> {
        Ok(Node::Number(v.to_string()))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Node, E> {
        Ok(Node::Number(v.to_string()))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Node, E> {
        Ok(Node::Number(serde_json::to_string(&v).unwrap_or_else(|_| "null".to_owned())))
    }

    fn visit_str<E>(self, v: &str) -> Result<Node, E> {
        Ok(Node::Str(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Node, E> {
        Ok(Node::Str(v))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Node, A::Error> {
        let mut items = Vec::new();
        while let Some(n) = seq.next_element_seed(NodeSeed)? {
            items.push(n);
        }
        Ok(Node::Arr(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Node, A::Error> {
        let mut entries = Vec::new();
        while let Some(k) = map.next_key::<String>()? {
            entries.push((k, map.next_value_seed(NodeSeed)?));
        }
        Ok(Node::Obj(entries))
    }
}

/// How strings are read.
#[derive(Clone, Copy)]
struct Reading {
    raw_text: bool,
}

/// A string as a cell: an ISO 8601 date or time becomes a date, an error's text the error, the rest text.
fn string_cell(s: &str, how: Reading) -> CellIn {
    if !how.raw_text {
        if let Some(e) = ErrorKind::parse(s.trim()).filter(|_| s.trim() == s) {
            return CellIn::plain(Value::Error(e));
        }
        if let Some((serial, fmt)) = parse_iso(s) {
            return CellIn { value: Value::Number(serial), num_fmt: Some(fmt), formula: None };
        }
    }
    CellIn::plain(Value::from(s))
}

/// `2026-10-17`, `2026-10-17T14:30:00` (with optional fraction and `Z`; an offset is ignored), `14:30:00`.
pub(crate) fn parse_iso(s: &str) -> Option<(f64, &'static str)> {
    let b = s.as_bytes();
    let digits = |r: std::ops::Range<usize>| -> Option<u32> {
        let part = s.get(r)?;
        if part.is_empty() || !part.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    let time_of = |t: &str| -> Option<f64> {
        let tb = t.as_bytes();
        if tb.len() < 5 || tb[2] != b':' {
            return None;
        }
        let h: u32 = t.get(0..2)?.parse().ok()?;
        let m: u32 = t.get(3..5)?.parse().ok()?;
        let mut sec = 0.0;
        let mut rest = &t[5..];
        if let Some(r) = rest.strip_prefix(':') {
            let end = r.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(r.len());
            sec = r[..end].parse().ok()?;
            rest = &r[end..];
        }
        let rest = rest.trim_end_matches('Z');
        if !(rest.is_empty() || ((rest.starts_with('+') || rest.starts_with('-')) && rest.len() <= 6)) {
            return None;
        }
        if h > 23 || m > 59 || sec >= 60.0 {
            return None;
        }
        Some((h as f64 * 3600.0 + m as f64 * 60.0 + sec) / 86400.0)
    };
    if b.len() >= 10 && b[4] == b'-' && b[7] == b'-' {
        let (y, mo, d) = (digits(0..4)?, digits(5..7)?, digits(8..10)?);
        if !(1..=12).contains(&mo) || d < 1 || d > crate::dates::days_in_month(y as i64, mo) {
            return None;
        }
        let day = crate::dates::serial_from_ymd(y as i64, mo as i64, d as i64)?;
        if b.len() == 10 {
            return Some((day, "yyyy-mm-dd"));
        }
        if b[10] != b'T' && b[10] != b' ' {
            return None;
        }
        let t = time_of(&s[11..])?;
        return Some((day + t, "yyyy-mm-dd hh:mm:ss"));
    }
    if b.len() >= 8 && b[2] == b':' && b[5] == b':' {
        return time_of(s).map(|t| (t, "hh:mm:ss"));
    }
    None
}

/// Whole numbers beyond 15 digits lose digits as numbers; they stay text.
fn integer_cell(text: String, n: f64) -> CellIn {
    if n.abs() >= 1e15 { CellIn::plain(Value::from(text.as_str())) } else { CellIn::plain(Value::Number(n)) }
}

/// One cell: a single value, `{"v", "f"}`, or something nested (kept as its JSON text).
struct CellSeed {
    how: Reading,
}

impl<'de> DeserializeSeed<'de> for CellSeed {
    type Value = CellIn;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<CellIn, D::Error> {
        d.deserialize_any(CellVisitor { how: self.how })
    }
}

struct CellVisitor {
    how: Reading,
}

impl<'de> Visitor<'de> for CellVisitor {
    type Value = CellIn;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a cell value")
    }

    fn visit_unit<E>(self) -> Result<CellIn, E> {
        Ok(CellIn::plain(Value::Empty))
    }

    fn visit_none<E>(self) -> Result<CellIn, E> {
        Ok(CellIn::plain(Value::Empty))
    }

    fn visit_bool<E>(self, v: bool) -> Result<CellIn, E> {
        Ok(CellIn::plain(Value::Bool(v)))
    }

    fn visit_i64<E>(self, v: i64) -> Result<CellIn, E> {
        Ok(integer_cell(v.to_string(), v as f64))
    }

    fn visit_u64<E>(self, v: u64) -> Result<CellIn, E> {
        Ok(integer_cell(v.to_string(), v as f64))
    }

    fn visit_f64<E>(self, v: f64) -> Result<CellIn, E> {
        Ok(CellIn::plain(if v.is_finite() { Value::Number(v) } else { Value::Error(ErrorKind::Num) }))
    }

    fn visit_str<E>(self, v: &str) -> Result<CellIn, E> {
        Ok(string_cell(v, self.how))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<CellIn, A::Error> {
        let node = NodeVisitor.visit_seq(seq)?;
        Ok(CellIn::plain(Value::from(node.json_text().as_str())))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<CellIn, A::Error> {
        let mut entries: Vec<(String, Node)> = Vec::new();
        while let Some(k) = map.next_key::<String>()? {
            entries.push((k, map.next_value_seed(NodeSeed)?));
        }
        // {"v": value, "f": "=formula"}: a cell as Calc writes it.
        let is_cell = !entries.is_empty()
            && entries.iter().all(|(k, _)| k == "v" || k == "f")
            && entries.iter().all(|(k, n)| k != "f" || matches!(n, Node::Str(_)));
        if !is_cell {
            return Ok(CellIn::plain(Value::from(Node::Obj(entries).json_text().as_str())));
        }
        let mut cell = CellIn::plain(Value::Empty);
        for (k, n) in entries {
            match (k.as_str(), n) {
                ("f", Node::Str(f)) => {
                    let t = f.trim();
                    let t = t.strip_prefix('=').unwrap_or(t).trim();
                    if !t.is_empty() {
                        cell.formula = Some(t.to_owned());
                    }
                }
                ("v", Node::Null) => {}
                ("v", Node::Bool(b)) => cell.value = Value::Bool(b),
                ("v", Node::Number(text)) => {
                    let n: f64 = text.parse().unwrap_or(0.0);
                    let c = if text.contains(['.', 'e', 'E']) { CellIn::plain(Value::Number(n)) } else { integer_cell(text, n) };
                    cell.value = c.value;
                }
                ("v", Node::Str(s)) => {
                    let c = string_cell(&s, self.how);
                    cell.value = c.value;
                    cell.num_fmt = c.num_fmt;
                }
                ("v", other) => cell.value = Value::from(other.json_text().as_str()),
                _ => {}
            }
        }
        Ok(cell)
    }
}

/// One row: an array (values by position), an object (values by key, through the header row) or a single value.
struct RowSeed<'a> {
    tab: &'a mut TabBuilder,
    how: Reading,
}

impl<'de> DeserializeSeed<'de> for RowSeed<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for RowSeed<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a row (an array or an object)")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let row = self.tab.rows;
        self.tab.saw_compound = true;
        let mut c = 0u32;
        while let Some(cell) = seq.next_element_seed(CellSeed { how: self.how })? {
            self.tab.set(row, c, cell).map_err(de::Error::custom)?;
            c += 1;
        }
        self.tab.rows = row + 1;
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        self.tab.saw_compound = true;
        if !self.tab.has_header && self.tab.rows == 0 {
            self.tab.has_header = true;
            self.tab.rows = 1;
        }
        let row = self.tab.rows;
        while let Some(k) = map.next_key::<String>()? {
            let c = self.tab.key_col(&k).map_err(de::Error::custom)?;
            let cell = map.next_value_seed(CellSeed { how: self.how })?;
            self.tab.set(row, c, cell).map_err(de::Error::custom)?;
        }
        self.tab.rows = row + 1;
        Ok(())
    }

    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        self.scalar(CellIn::plain(Value::Empty))
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<(), E> {
        self.scalar(CellIn::plain(Value::Bool(v)))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<(), E> {
        self.scalar(integer_cell(v.to_string(), v as f64))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<(), E> {
        self.scalar(integer_cell(v.to_string(), v as f64))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<(), E> {
        self.scalar(CellIn::plain(Value::Number(v)))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<(), E> {
        let how = self.how;
        self.scalar(string_cell(v, how))
    }
}

impl RowSeed<'_> {
    fn scalar<E: de::Error>(self, cell: CellIn) -> Result<(), E> {
        let row = self.tab.rows;
        self.tab.saw_scalar = true;
        self.tab.set(row, 0, cell).map_err(E::custom)?;
        self.tab.rows = row + 1;
        Ok(())
    }
}

/// A list of rows (or, given an object, one object row).
struct RowsSeed<'a> {
    tab: &'a mut TabBuilder,
    how: Reading,
}

impl<'de> DeserializeSeed<'de> for RowsSeed<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for RowsSeed<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("rows")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let how = self.how;
        while seq.next_element_seed(RowSeed { tab: &mut *self.tab, how })?.is_some() {}
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<(), A::Error> {
        RowSeed { tab: self.tab, how: self.how }.visit_map(map)
    }

    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        RowSeed { tab: self.tab, how: self.how }.visit_unit()
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<(), E> {
        RowSeed { tab: self.tab, how: self.how }.visit_bool(v)
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<(), E> {
        RowSeed { tab: self.tab, how: self.how }.visit_i64(v)
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<(), E> {
        RowSeed { tab: self.tab, how: self.how }.visit_u64(v)
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<(), E> {
        RowSeed { tab: self.tab, how: self.how }.visit_f64(v)
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<(), E> {
        RowSeed { tab: self.tab, how: self.how }.visit_str(v)
    }
}

/// Everything read so far: tabs in order, and the single values of a top-level object (collected into one row).
struct Out {
    tabs: Vec<TabBuilder>,
    /// Index into `tabs` of the tab collecting a top-level object's single values.
    scalars: Option<usize>,
    /// The tabs made one per key of a top-level object, by index into `tabs`.
    keyed: Vec<usize>,
    how: Reading,
}

impl Out {
    fn scalar_tab(&mut self) -> &mut TabBuilder {
        let i = match self.scalars {
            Some(i) => i,
            None => {
                self.tabs.push(TabBuilder::new("Tab 1"));
                let i = self.tabs.len() - 1;
                self.scalars = Some(i);
                i
            }
        };
        &mut self.tabs[i]
    }
}

/// The `tabs` array of Calc's own shape. An element without `rows` is read as an object row of a tab named
/// `tabs`, so a file that merely has a `tabs` key still opens.
struct TabsSeed<'a> {
    out: &'a mut Out,
}

impl<'de> DeserializeSeed<'de> for TabsSeed<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for TabsSeed<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a list of tabs")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let mut fallback: Option<usize> = None;
        while let Some(spec) = seq.next_element_seed(TabSpecSeed { how: self.out.how })? {
            match spec {
                TabSpec::Tab(t) => self.out.tabs.push(t),
                TabSpec::Row(entries) => {
                    let i = *fallback.get_or_insert_with(|| {
                        self.out.tabs.push(TabBuilder::new("tabs"));
                        self.out.tabs.len() - 1
                    });
                    let tab = &mut self.out.tabs[i];
                    if !tab.has_header && tab.rows == 0 {
                        tab.has_header = true;
                        tab.rows = 1;
                    }
                    let row = tab.rows;
                    for (k, n) in entries {
                        let c = tab.key_col(&k).map_err(de::Error::custom)?;
                        tab.set(row, c, node_cell(n, self.out.how)).map_err(de::Error::custom)?;
                    }
                    tab.rows = row + 1;
                }
                TabSpec::Other(n) => {
                    let i = *fallback.get_or_insert_with(|| {
                        self.out.tabs.push(TabBuilder::new("tabs"));
                        self.out.tabs.len() - 1
                    });
                    let tab = &mut self.out.tabs[i];
                    let row = tab.rows;
                    tab.set(row, 0, node_cell(n, self.out.how)).map_err(de::Error::custom)?;
                    tab.rows = row + 1;
                }
            }
        }
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<(), A::Error> {
        let how = self.out.how;
        self.out.tabs.push(TabBuilder::new("tabs"));
        let i = self.out.tabs.len() - 1;
        self.out.keyed.push(i);
        RowSeed { tab: &mut self.out.tabs[i], how }.visit_map(map)
    }

    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        self.scalar(Node::Null)
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<(), E> {
        self.scalar(Node::Bool(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<(), E> {
        self.scalar(Node::Number(v.to_string()))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<(), E> {
        self.scalar(Node::Number(v.to_string()))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<(), E> {
        self.scalar(Node::Number(serde_json::to_string(&v).unwrap_or_default()))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<(), E> {
        self.scalar(Node::Str(v.to_owned()))
    }
}

impl TabsSeed<'_> {
    fn scalar<E: de::Error>(self, n: Node) -> Result<(), E> {
        let how = self.out.how;
        let tab = self.out.scalar_tab();
        let c = tab.key_col("tabs").map_err(E::custom)?;
        let row = tab.rows.max(1);
        tab.set(row, c, node_cell(n, how)).map_err(E::custom)?;
        tab.rows = row + 1;
        Ok(())
    }
}

/// A nested value as a cell.
fn node_cell(n: Node, how: Reading) -> CellIn {
    match n {
        Node::Null => CellIn::plain(Value::Empty),
        Node::Bool(b) => CellIn::plain(Value::Bool(b)),
        Node::Number(text) => {
            let v: f64 = text.parse().unwrap_or(0.0);
            if text.contains(['.', 'e', 'E']) { CellIn::plain(Value::Number(v)) } else { integer_cell(text, v) }
        }
        Node::Str(s) => string_cell(&s, how),
        other => CellIn::plain(Value::from(other.json_text().as_str())),
    }
}

enum TabSpec {
    Tab(TabBuilder),
    /// An object without `rows`: one row of a table.
    Row(Vec<(String, Node)>),
    Other(Node),
}

struct TabSpecSeed {
    how: Reading,
}

impl<'de> DeserializeSeed<'de> for TabSpecSeed {
    type Value = TabSpec;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<TabSpec, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for TabSpecSeed {
    type Value = TabSpec;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a tab: {\"name\": …, \"rows\": […]}")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<TabSpec, A::Error> {
        let mut tab = TabBuilder::new("");
        let mut has_rows = false;
        let mut others: Vec<(String, Node)> = Vec::new();
        while let Some(k) = map.next_key::<String>()? {
            if k == "rows" && !has_rows {
                has_rows = true;
                map.next_value_seed(RowsSeed { tab: &mut tab, how: self.how })?;
            } else {
                others.push((k, map.next_value_seed(NodeSeed)?));
            }
        }
        if !has_rows {
            return Ok(TabSpec::Row(others));
        }
        if let Some((_, Node::Str(name))) = others.iter().find(|(k, n)| k == "name" && matches!(n, Node::Str(_))) {
            tab.name = name.clone();
        }
        Ok(TabSpec::Tab(tab))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<TabSpec, A::Error> {
        Ok(TabSpec::Other(NodeVisitor.visit_seq(seq)?))
    }

    fn visit_unit<E>(self) -> Result<TabSpec, E> {
        Ok(TabSpec::Other(Node::Null))
    }

    fn visit_bool<E>(self, v: bool) -> Result<TabSpec, E> {
        Ok(TabSpec::Other(Node::Bool(v)))
    }

    fn visit_i64<E>(self, v: i64) -> Result<TabSpec, E> {
        Ok(TabSpec::Other(Node::Number(v.to_string())))
    }

    fn visit_u64<E>(self, v: u64) -> Result<TabSpec, E> {
        Ok(TabSpec::Other(Node::Number(v.to_string())))
    }

    fn visit_f64<E>(self, v: f64) -> Result<TabSpec, E> {
        Ok(TabSpec::Other(Node::Number(serde_json::to_string(&v).unwrap_or_default())))
    }

    fn visit_str<E>(self, v: &str) -> Result<TabSpec, E> {
        Ok(TabSpec::Other(Node::Str(v.to_owned())))
    }
}

/// A top-level object's value: a list or object becomes a tab named by its key; a single value joins the row of
/// single values.
struct KeyedSeed<'a> {
    out: &'a mut Out,
    key: String,
}

impl<'de> DeserializeSeed<'de> for KeyedSeed<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for KeyedSeed<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a value")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<(), A::Error> {
        let how = self.out.how;
        self.out.tabs.push(TabBuilder::new(self.key));
        let i = self.out.tabs.len() - 1;
        self.out.keyed.push(i);
        RowsSeed { tab: &mut self.out.tabs[i], how }.visit_seq(seq)
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<(), A::Error> {
        let how = self.out.how;
        self.out.tabs.push(TabBuilder::new(self.key));
        let i = self.out.tabs.len() - 1;
        self.out.keyed.push(i);
        RowSeed { tab: &mut self.out.tabs[i], how }.visit_map(map)
    }

    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        self.scalar(CellIn::plain(Value::Empty))
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<(), E> {
        self.scalar(CellIn::plain(Value::Bool(v)))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<(), E> {
        self.scalar(integer_cell(v.to_string(), v as f64))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<(), E> {
        self.scalar(integer_cell(v.to_string(), v as f64))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<(), E> {
        self.scalar(CellIn::plain(Value::Number(v)))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<(), E> {
        let how = self.out.how;
        self.scalar(string_cell(v, how))
    }
}

impl KeyedSeed<'_> {
    fn scalar<E: de::Error>(self, cell: CellIn) -> Result<(), E> {
        let tab = self.out.scalar_tab();
        let c = tab.key_col(&self.key).map_err(E::custom)?;
        tab.set(1, c, cell).map_err(E::custom)?;
        tab.rows = tab.rows.max(2);
        Ok(())
    }
}

/// The whole document.
struct TopSeed<'a> {
    out: &'a mut Out,
}

impl<'de> DeserializeSeed<'de> for TopSeed<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for TopSeed<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON array or object")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<(), A::Error> {
        let how = self.out.how;
        self.out.tabs.push(TabBuilder::new("Tab 1"));
        let i = self.out.tabs.len() - 1;
        RowsSeed { tab: &mut self.out.tabs[i], how }.visit_seq(seq)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(k) = map.next_key::<String>()? {
            if k == "tabs" {
                map.next_value_seed(TabsSeed { out: &mut *self.out })?;
            } else {
                map.next_value_seed(KeyedSeed { out: &mut *self.out, key: k })?;
            }
        }
        Ok(())
    }

    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<(), E> {
        self.single(CellIn::plain(Value::Bool(v)))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<(), E> {
        self.single(integer_cell(v.to_string(), v as f64))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<(), E> {
        self.single(integer_cell(v.to_string(), v as f64))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<(), E> {
        self.single(CellIn::plain(Value::Number(v)))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<(), E> {
        let how = self.out.how;
        self.single(string_cell(v, how))
    }
}

impl TopSeed<'_> {
    fn single<E: de::Error>(self, cell: CellIn) -> Result<(), E> {
        let mut t = TabBuilder::new("Tab 1");
        t.set(0, 0, cell).map_err(E::custom)?;
        t.rows = 1;
        self.out.tabs.push(t);
        Ok(())
    }
}

/// `{"name": [...], "age": [...]}`: one table with the keys as its header row, when every value of the object
/// was a list of single values (and there was nothing else).
fn merge_columns(out: &mut Out) {
    if out.keyed.len() < 2 || out.scalars.is_some() || out.keyed.len() != out.tabs.len() {
        return;
    }
    let columnar = out.tabs.iter().all(|t| t.saw_scalar && !t.saw_compound && t.columns.len() <= 1 && t.rows > 0);
    if !columnar {
        return;
    }
    let tabs = std::mem::take(&mut out.tabs);
    let mut merged = TabBuilder::new("Tab 1");
    merged.rows = 1;
    for (c, t) in tabs.into_iter().enumerate() {
        let c = c as u32;
        let _ = merged.set(0, c, CellIn::plain(Value::from(t.name.as_str())));
        merged.rows = merged.rows.max(t.rows + 1);
        let (_, content) = t.finish();
        let Some(col) = content.columns.into_iter().next() else { continue };
        for (r, v) in col.values.into_iter().enumerate() {
            if !v.is_empty() {
                let _ = merged.set(r as u32 + 1, c, CellIn::plain(v));
            }
        }
        for (r, s) in col.styles {
            if let Some(code) = content.styles.get(s as usize).and_then(|s| s.num_fmt.clone()) {
                let fmt: &'static str = match code.as_str() {
                    "yyyy-mm-dd" => "yyyy-mm-dd",
                    "hh:mm:ss" => "hh:mm:ss",
                    _ => "yyyy-mm-dd hh:mm:ss",
                };
                let next = merged.styles.len() as u32;
                let ix = *merged.style_of.entry(fmt).or_insert_with(|| {
                    merged.styles.push(Style { num_fmt: Some(fmt.to_owned()), ..Default::default() });
                    next
                });
                merged.columns[c as usize].styles.push((r + 1, ix));
            }
        }
    }
    out.keyed = Vec::new();
    out.tabs.push(merged);
}

fn json_error(e: serde_json::Error) -> FormatError {
    let msg = e.to_string();
    let msg = msg.trim_end_matches('.').to_owned();
    FormatError::read(FORMAT, msg)
}

pub(crate) fn import(bytes: &[u8], opts: &ImportOptions) -> Result<Imported, FormatError> {
    let (text, encoding) = decode_text(bytes);
    let text = text.trim_start_matches('\u{feff}');
    if text.trim().is_empty() {
        return Err(FormatError::read(FORMAT, "the file is empty"));
    }
    let how = Reading { raw_text: opts.raw_text };
    let mut out = Out { tabs: Vec::new(), scalars: None, keyed: Vec::new(), how };
    let mut de = serde_json::Deserializer::from_str(text);
    let whole = TopSeed { out: &mut out }.deserialize(&mut de).and_then(|_| de.end());
    if let Err(e) = whole {
        // JSON Lines: one value per line.
        let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        let first_ok = lines.len() > 1 && serde_json::from_str::<serde::de::IgnoredAny>(lines[0]).is_ok();
        if !first_ok {
            return Err(json_error(e));
        }
        out = Out { tabs: vec![TabBuilder::new("Tab 1")], scalars: None, keyed: Vec::new(), how };
        for (n, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let mut de = serde_json::Deserializer::from_str(line);
            RowSeed { tab: &mut out.tabs[0], how }
                .deserialize(&mut de)
                .and_then(|_| de.end())
                .map_err(|e| FormatError::read(FORMAT, format!("line {} isn't JSON ({})", n + 1, e.to_string().trim_end_matches('.'))))?;
        }
    }
    merge_columns(&mut out);
    let mut tabs: Vec<(String, TabContent)> = Vec::new();
    for t in out.tabs {
        let fallback = format!("Tab {}", tabs.len() + 1);
        let (name, content) = t.finish();
        tabs.push((if name.trim().is_empty() { fallback } else { name }, content));
    }
    if tabs.is_empty() {
        tabs.push(("Tab 1".to_owned(), TabContent::default()));
    }
    let mut warnings = Vec::new();
    if encoding != "UTF-8" {
        warnings.push(format!("The file was read as {encoding} text."));
    }
    let tab_info = vec![Default::default(); tabs.len()];
    Ok(Imported { tabs, warnings, tab_info, ..Default::default() })
}

// ---- writing ----------------------------------------------------------------------------------------------------

/// A date, time or both as ISO 8601 text, per the cell's format. None for serials that aren't real dates.
pub(crate) fn iso_text(serial: f64, kind: DateKind) -> Option<String> {
    let time = || {
        let (h, m, s, ms) = hms_from_serial(serial);
        if ms >= 500 && s < 59 {
            // Seconds are rounded the way the cell would show them.
            format!("{h:02}:{m:02}:{:02}", s + 1)
        } else {
            format!("{h:02}:{m:02}:{s:02}")
        }
    };
    let date = || -> Option<String> {
        let (y, m, d) = ymd_from_serial(serial)?;
        if d == 0 || (y == 1900 && m == 2 && d == 29) {
            return None;
        }
        Some(format!("{y:04}-{m:02}-{d:02}"))
    };
    match kind {
        DateKind::Date => date(),
        DateKind::Time | DateKind::Duration if (0.0..1.0).contains(&serial) => Some(time()),
        DateKind::Time | DateKind::Duration => None,
        DateKind::DateTime => Some(format!("{}T{}", date()?, time())),
    }
}

/// A cell's value as JSON.
fn push_value(out: &mut Vec<u8>, v: &Value, num_fmt: Option<&str>) {
    match v {
        Value::Empty => out.extend_from_slice(b"null"),
        Value::Number(n) => {
            if let Some(iso) = num_fmt.and_then(date_kind).and_then(|k| iso_text(*n, k)) {
                write_str(out, &iso);
            } else if n.is_finite() {
                out.extend_from_slice(number_text(*n).as_bytes());
            } else {
                write_str(out, "#NUM!");
            }
        }
        Value::Text(s) => write_str(out, s),
        Value::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Value::Error(e) => write_str(out, e.as_str()),
    }
}

fn write_str(out: &mut Vec<u8>, s: &str) {
    let _ = serde_json::to_writer(&mut *out, s);
}

fn write_tab(out: &mut Vec<u8>, wb: &Workbook, tab: &Tab, values_only: bool) {
    out.extend_from_slice(b"{\"name\":");
    write_str(out, &tab.name);
    out.extend_from_slice(b",\"rows\":[");
    if let Some(used) = tab.used_rect() {
        for r in 0..=used.r2 {
            out.extend_from_slice(if r == 0 { b"\n[" } else { b",\n[" });
            for c in 0..=used.c2 {
                if c > 0 {
                    out.push(b',');
                }
                let cell = tab.cell_at(CellPos::new(r, c));
                let fmt = wb.style_of(cell.style).num_fmt.as_deref();
                let formula = if values_only || cell.spilled { None } else { wb.formula_text(tab.id, r, c) };
                match formula {
                    Some(f) => {
                        out.extend_from_slice(b"{\"v\":");
                        push_value(out, &cell.value, fmt);
                        out.extend_from_slice(b",\"f\":");
                        write_str(out, &format!("={f}"));
                        out.push(b'}');
                    }
                    None => push_value(out, &cell.value, fmt),
                }
            }
            out.push(b']');
        }
        out.push(b'\n');
    }
    out.extend_from_slice(b"]}");
}

pub(crate) fn export(wb: &Workbook, opts: &ExportOptions) -> Result<Vec<u8>, FormatError> {
    let tabs = export_tabs(wb, opts, FORMAT)?;
    let mut out = Vec::with_capacity(4096);
    out.extend_from_slice(b"{\"tabs\":[\n");
    for (i, &t) in tabs.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(b",\n");
        }
        let tab = wb.tab(t).expect("listed");
        write_tab(&mut out, wb, tab, opts.values_only);
    }
    out.extend_from_slice(b"\n]}\n");
    out.flush().map_err(|e| FormatError::write(FORMAT, e.to_string()))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(s: &str) -> Imported {
        import(s.as_bytes(), &ImportOptions::default()).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    fn cell(c: &TabContent, r: usize, col: usize) -> Value {
        c.columns.get(col).and_then(|c| c.values.get(r)).cloned().unwrap_or_default()
    }

    #[test]
    fn array_of_objects_keeps_key_order() {
        let i = read(r#"[{"zeta": 1, "alpha": "a"}, {"alpha": "b", "mid": true, "zeta": 2}]"#);
        let (_, c) = &i.tabs[0];
        assert_eq!(c.rows, 3);
        assert_eq!(cell(c, 0, 0), Value::from("zeta"));
        assert_eq!(cell(c, 0, 1), Value::from("alpha"));
        assert_eq!(cell(c, 0, 2), Value::from("mid"));
        assert_eq!(cell(c, 1, 0), Value::Number(1.0));
        assert_eq!(cell(c, 2, 1), Value::from("b"));
        assert_eq!(cell(c, 2, 2), Value::Bool(true));
        assert_eq!(cell(c, 1, 2), Value::Empty);
    }

    #[test]
    fn grids_columns_and_keys() {
        let i = read("[[1, 2], [\"a\", null, 3]]");
        assert_eq!(i.tabs[0].1.cols, 3);
        assert_eq!(cell(&i.tabs[0].1, 1, 2), Value::Number(3.0));
        let i = read(r#"{"Sales": [{"a": 1}], "Costs": [[5, 6]]}"#);
        assert_eq!(i.tabs.iter().map(|t| t.0.as_str()).collect::<Vec<_>>(), ["Sales", "Costs"]);
        assert_eq!(cell(&i.tabs[1].1, 0, 1), Value::Number(6.0));
        let i = read(r#"{"name": ["ada", "bob"], "age": [36, 41]}"#);
        assert_eq!(i.tabs.len(), 1);
        assert_eq!(cell(&i.tabs[0].1, 0, 1), Value::from("age"));
        assert_eq!(cell(&i.tabs[0].1, 2, 1), Value::Number(41.0));
        let i = read(r#"{"a": 1, "b": "x"}"#);
        assert_eq!(cell(&i.tabs[0].1, 0, 1), Value::from("b"));
        assert_eq!(cell(&i.tabs[0].1, 1, 0), Value::Number(1.0));
    }

    #[test]
    fn values_dates_and_nesting() {
        let i = read(r##"[["2026-10-17", "2026-10-17T14:30:00Z", "#N/A", 12345678901234567890, {"k": [1, {"z": 2}]}, "12"]]"##);
        let c = &i.tabs[0].1;
        assert_eq!(cell(c, 0, 0), Value::Number(46312.0));
        assert_eq!(c.styles[c.columns[0].styles[0].1 as usize].num_fmt.as_deref(), Some("yyyy-mm-dd"));
        assert_eq!(cell(c, 0, 1), Value::Number(46312.0 + 14.5 / 24.0));
        assert_eq!(cell(c, 0, 2), Value::Error(ErrorKind::NA));
        assert_eq!(cell(c, 0, 3), Value::from("12345678901234567890"));
        assert_eq!(cell(c, 0, 4), Value::from(r#"{"k":[1,{"z":2}]}"#));
        assert_eq!(cell(c, 0, 5), Value::from("12"));
    }

    #[test]
    fn json_lines() {
        let i = read("{\"a\": 1}\n{\"a\": 2, \"b\": 3}\n");
        let c = &i.tabs[0].1;
        assert_eq!(c.rows, 3);
        assert_eq!(cell(c, 2, 1), Value::Number(3.0));
    }

    #[test]
    fn calc_shape_with_formulas() {
        let i = read(r#"{"tabs":[{"name":"S","rows":[[1,2,{"v":3,"f":"=A1+B1"}]]},{"name":"T","rows":[]}]}"#);
        assert_eq!(i.tabs.len(), 2);
        let c = &i.tabs[0].1;
        assert_eq!(cell(c, 0, 2), Value::Number(3.0));
        assert_eq!(c.columns[2].formulas, vec![(0, "A1+B1".to_owned())]);
        assert!(import(b"{\"a\": ", &ImportOptions::default()).is_err());
    }

    #[test]
    fn iso() {
        assert_eq!(parse_iso("2026-10-17"), Some((46312.0, "yyyy-mm-dd")));
        assert_eq!(parse_iso("14:30:00"), Some((14.5 / 24.0, "hh:mm:ss")));
        assert_eq!(parse_iso("2026-02-30"), None);
        assert_eq!(parse_iso("2026-10-17x"), None);
        assert_eq!(iso_text(46312.75, DateKind::DateTime).as_deref(), Some("2026-10-17T18:00:00"));
        assert_eq!(iso_text(46312.0, DateKind::Date).as_deref(), Some("2026-10-17"));
        assert_eq!(iso_text(0.5, DateKind::Time).as_deref(), Some("12:00:00"));
    }
}
