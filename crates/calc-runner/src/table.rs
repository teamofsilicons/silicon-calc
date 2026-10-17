//! A tab, a range of one (`Sales!A1:C100`) or a defined name, read as a table: its first row names the columns,
//! and each column gets the type its cells share (whole numbers, numbers, text, true/false, dates, times).
//!
//! This is what SQL queries, `calc.tab()` in Python and JavaScript, and Parquet downloads all see.

use std::{collections::HashSet, sync::Arc};

use calc_engine::{
    CellPos, Rect, TabId, Value, Workbook,
    addr::{MAX_COLS, MAX_ROWS, split_tab},
    numfmt,
};
use duckdb::arrow::{
    array::{
        ArrayRef, BooleanBuilder, Date32Builder, Float64Builder, Int64Builder, StringBuilder, Time64MicrosecondBuilder,
        TimestampMicrosecondBuilder,
    },
    datatypes::{DataType, Field, Schema, TimeUnit},
    record_batch::RecordBatch,
};
use serde::Serialize;

use crate::values::{date_parts, day_micros_from_serial, unix_days_from_serial, unix_micros_from_serial};

/// The type a table column gets from its cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ColumnType {
    /// Every value is a whole number.
    Integer,
    Number,
    Text,
    Bool,
    /// Every value is a number formatted as a date.
    Date,
    /// Every value is a number formatted as a date with a time (or has a time part).
    Timestamp,
    /// Every value is a time of day.
    Time,
}

impl ColumnType {
    /// The SQL type the column has in DuckDB.
    pub fn sql(self) -> &'static str {
        match self {
            Self::Integer => "BIGINT",
            Self::Number => "DOUBLE",
            Self::Text => "VARCHAR",
            Self::Bool => "BOOLEAN",
            Self::Date => "DATE",
            Self::Timestamp => "TIMESTAMP",
            Self::Time => "TIME",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Integer => "integer",
            Self::Number => "number",
            Self::Text => "text",
            Self::Bool => "bool",
            Self::Date => "date",
            Self::Timestamp => "timestamp",
            Self::Time => "time",
        }
    }

    fn arrow(self) -> DataType {
        match self {
            Self::Integer => DataType::Int64,
            Self::Number => DataType::Float64,
            Self::Text => DataType::Utf8,
            Self::Bool => DataType::Boolean,
            Self::Date => DataType::Date32,
            Self::Timestamp => DataType::Timestamp(TimeUnit::Microsecond, None),
            Self::Time => DataType::Time64(TimeUnit::Microsecond),
        }
    }
}

/// Where a table's cells come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The whole tab (from A1 to the last cell in use).
    Tab(TabId),
    /// A range of a tab, by position.
    Range(TabId, Rect),
}

impl Source {
    pub fn tab(&self) -> TabId {
        match self {
            Self::Tab(t) | Self::Range(t, _) => *t,
        }
    }
}

/// What `name` names in `wb`: a tab (case-insensitive), a range (`Sales!A1:C9`, `'Q3 plan'!B:D`), or a defined
/// name that stands for a range.
pub fn resolve(wb: &Workbook, name: &str) -> Option<Source> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    if let Some(t) = wb.tab_by_name(name) {
        return Some(Source::Tab(t.id));
    }
    let (tab, rest) = split_tab(name);
    if let Some(tab) = tab {
        let t = wb.tab_by_name(&tab)?;
        let rect = Rect::parse(rest, MAX_ROWS, MAX_COLS)?;
        return Some(Source::Range(t.id, rect));
    }
    let def = wb.names.iter().find(|n| n.name.eq_ignore_ascii_case(name))?;
    let home = def.tab.or_else(|| wb.tabs.first().map(|t| t.id))?;
    let area = wb.resolve_area(def.text.trim_start_matches('='), home)?;
    wb.tab(area.tab)?;
    Some(Source::Range(area.tab, area.rect))
}

/// A tab or range read as a table: column names, their types, and the rows below the first as Arrow arrays.
#[derive(Clone, Debug)]
pub struct Table {
    /// The name it is known by (as the code wrote it, or the tab's own name).
    pub name: String,
    pub tab: TabId,
    pub columns: Vec<String>,
    pub types: Vec<ColumnType>,
    pub batch: RecordBatch,
}

impl Table {
    pub fn rows(&self) -> usize {
        self.batch.num_rows()
    }
}

/// The area of a tab a source covers, by position: whole columns and rows stop at the last cell in use, and
/// trailing rows with nothing in them are left out. None when there is nothing at all.
fn area_of(wb: &Workbook, source: &Source) -> Option<Rect> {
    let t = wb.tab(source.tab())?;
    let used = t.used_rect();
    match source {
        Source::Tab(_) => used.map(|u| Rect::new(0, 0, u.r2, u.c2)),
        Source::Range(_, r) => {
            let whole_rows = r.c1 == 0 && r.c2 >= MAX_COLS - 1;
            let r2 = match used {
                Some(u) => r.r2.min(u.r2.max(r.r1)),
                None => r.r1,
            };
            let c2 = if whole_rows {
                match used {
                    Some(u) => u.c2.max(r.c1),
                    None => r.c1,
                }
            } else {
                r.c2
            };
            Some(Rect::new(r.r1, r.c1, r2, c2))
        }
    }
}

/// What a cell shows, as text (numbers in their format).
pub(crate) fn shown_text(wb: &Workbook, value: &Value, style: u32) -> String {
    match value {
        Value::Number(_) => numfmt::format_value(value, wb.style_of(style).num_fmt.as_deref()).0,
        other => other.display(),
    }
}

/// Column names from a header row: trimmed, blanks named `column1`, `column2`, … by position, repeats (compared
/// without case, as SQL compares them) numbered `name_1`, `name_2`, ….
pub fn column_names(raw: &[String]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(raw.len());
    for (i, n) in raw.iter().enumerate() {
        let clean: String = n.trim().chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        let base = if clean.is_empty() { format!("column{}", i + 1) } else { clean };
        let mut name = base.clone();
        let mut k = 1;
        while seen.contains(&name.to_lowercase()) {
            name = format!("{base}_{k}");
            k += 1;
        }
        seen.insert(name.to_lowercase());
        out.push(name);
    }
    out
}

#[derive(Default)]
struct Stats {
    numbers: usize,
    texts: usize,
    bools: usize,
    dated: usize,
    integral: bool,
    date_part: bool,
    time_part: bool,
    fractional: bool,
    within_day: bool,
}

/// Whether a style's number format shows dates, and its parts. Cached per style id.
struct Formats<'a> {
    wb: &'a Workbook,
    cache: std::collections::HashMap<u32, Option<(bool, bool)>>,
}

impl<'a> Formats<'a> {
    fn new(wb: &'a Workbook) -> Self {
        Self { wb, cache: Default::default() }
    }

    /// `Some((date part, time part))` when the style formats numbers as dates or times.
    fn date(&mut self, style: u32) -> Option<(bool, bool)> {
        if style == 0 {
            return None;
        }
        let wb = self.wb;
        *self.cache.entry(style).or_insert_with(|| {
            let f = wb.style_of(style).num_fmt.as_deref()?;
            numfmt::is_date_format(f).then(|| date_parts(f))
        })
    }
}

fn infer(stats: &Stats) -> ColumnType {
    let s = stats;
    if s.texts == 0 && s.bools == 0 && s.numbers > 0 {
        if s.dated == s.numbers {
            if !s.date_part && s.within_day {
                return ColumnType::Time;
            }
            if s.time_part || s.fractional {
                return ColumnType::Timestamp;
            }
            return ColumnType::Date;
        }
        if s.integral {
            return ColumnType::Integer;
        }
        return ColumnType::Number;
    }
    if s.numbers == 0 && s.texts == 0 && s.bools > 0 {
        return ColumnType::Bool;
    }
    ColumnType::Text
}

/// Reads a source of `wb` as a table named `name`.
pub fn build(wb: &Workbook, name: &str, source: &Source) -> Table {
    let tab_id = source.tab();
    let Some((t, area)) = wb.tab(tab_id).zip(area_of(wb, source)) else {
        return empty_table(name, tab_id);
    };
    let mut formats = Formats::new(wb);
    let header: Vec<String> = (area.c1..=area.c2)
        .map(|c| {
            let cell = t.cell_at(CellPos::new(area.r1, c));
            shown_text(wb, &cell.value, cell.style)
        })
        .collect();
    let names = column_names(&header);
    let first = area.r1 + 1;
    let rows = if area.r2 >= first { (area.r2 - first + 1) as usize } else { 0 };
    let row_ids: Vec<Option<u32>> = (0..rows as u32).map(|i| t.rows.id_at(first + i)).collect();
    let mut fields = Vec::with_capacity(names.len());
    let mut arrays: Vec<ArrayRef> = Vec::with_capacity(names.len());
    let mut types = Vec::with_capacity(names.len());
    for (i, c) in (area.c1..=area.c2).enumerate() {
        let col_id = t.cols.id_at(c);
        let cell = |r: usize| match (row_ids[r], col_id) {
            (Some(rid), Some(cid)) => t.cell_by_id(rid, cid),
            _ => Default::default(),
        };
        // First pass: what the column holds.
        let mut s = Stats { integral: true, within_day: true, ..Default::default() };
        for r in 0..rows {
            let cell = cell(r);
            match &cell.value {
                Value::Number(n) => {
                    s.numbers += 1;
                    if n.fract() != 0.0 || n.abs() > 9_007_199_254_740_992.0 {
                        s.integral = false;
                    }
                    if n.fract() != 0.0 {
                        s.fractional = true;
                    }
                    if !(0.0..1.0).contains(n) {
                        s.within_day = false;
                    }
                    if let Some((d, tm)) = formats.date(cell.style) {
                        s.dated += 1;
                        s.date_part |= d;
                        s.time_part |= tm;
                    }
                }
                Value::Text(_) => s.texts += 1,
                Value::Bool(_) => s.bools += 1,
                Value::Error(_) | Value::Empty => {}
            }
        }
        let ty = infer(&s);
        // Second pass: the values.
        let array: ArrayRef = match ty {
            ColumnType::Integer => {
                let mut b = Int64Builder::with_capacity(rows);
                for r in 0..rows {
                    match cell(r).value {
                        Value::Number(n) => b.append_value(n as i64),
                        _ => b.append_null(),
                    }
                }
                Arc::new(b.finish())
            }
            ColumnType::Number => {
                let mut b = Float64Builder::with_capacity(rows);
                for r in 0..rows {
                    match cell(r).value {
                        Value::Number(n) => b.append_value(n),
                        _ => b.append_null(),
                    }
                }
                Arc::new(b.finish())
            }
            ColumnType::Bool => {
                let mut b = BooleanBuilder::with_capacity(rows);
                for r in 0..rows {
                    match cell(r).value {
                        Value::Bool(v) => b.append_value(v),
                        _ => b.append_null(),
                    }
                }
                Arc::new(b.finish())
            }
            ColumnType::Date => {
                let mut b = Date32Builder::with_capacity(rows);
                for r in 0..rows {
                    match cell(r).value {
                        Value::Number(n) => b.append_value(unix_days_from_serial(n).clamp(i32::MIN as i64, i32::MAX as i64) as i32),
                        _ => b.append_null(),
                    }
                }
                Arc::new(b.finish())
            }
            ColumnType::Timestamp => {
                let mut b = TimestampMicrosecondBuilder::with_capacity(rows);
                for r in 0..rows {
                    match cell(r).value {
                        Value::Number(n) => b.append_value(unix_micros_from_serial(n)),
                        _ => b.append_null(),
                    }
                }
                Arc::new(b.finish())
            }
            ColumnType::Time => {
                let mut b = Time64MicrosecondBuilder::with_capacity(rows);
                for r in 0..rows {
                    match cell(r).value {
                        Value::Number(n) => b.append_value(day_micros_from_serial(n)),
                        _ => b.append_null(),
                    }
                }
                Arc::new(b.finish())
            }
            ColumnType::Text => {
                let mut b = StringBuilder::with_capacity(rows, rows * 8);
                for r in 0..rows {
                    let cell = cell(r);
                    match &cell.value {
                        Value::Empty => b.append_null(),
                        Value::Text(s) => b.append_value(s),
                        other => b.append_value(shown_text(wb, other, cell.style)),
                    }
                }
                Arc::new(b.finish())
            }
        };
        fields.push(Field::new(names[i].clone(), ty.arrow(), true));
        arrays.push(array);
        types.push(ty);
    }
    let batch = RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays).expect("columns match their fields");
    Table { name: name.to_owned(), tab: tab_id, columns: names, types, batch }
}

/// A table with nothing in it: one text column, no rows (SQL tables need a column).
fn empty_table(name: &str, tab: TabId) -> Table {
    let fields = vec![Field::new("column1", DataType::Utf8, true)];
    let arrays: Vec<ArrayRef> = vec![Arc::new(StringBuilder::new().finish())];
    let batch = RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays).expect("one empty column");
    Table { name: name.to_owned(), tab, columns: vec!["column1".into()], types: vec![ColumnType::Text], batch }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        let raw = ["region", "", "Amount", "amount", " total ", "", "column2"].map(String::from);
        assert_eq!(column_names(&raw), vec!["region", "column2", "Amount", "amount_1", "total", "column6", "column2_1"]);
    }
}
