//! One worksheet part, streamed: cells (values with their types, styles, formulas), rows and columns (sizes,
//! hidden ones, default styles), merges, frozen panes, the tab colour and the drawing that holds its charts.

use std::{io::BufRead, sync::Arc};

use ahash::HashMap;
use quick_xml::{Reader, events::Event};

use crate::{
    addr::{CellPos, MAX_COLS, MAX_ROWS, Rect},
    dates::parse_date_time,
    formats::{
        FormatError, IndexNames, Warnings,
        colors::{ColorSpec, Palette},
        xml::{attr, attr_f64, attr_raw, attr_u32, local, name_of, push_text, reader, skip_element, truthy},
    },
    formula::{ast::Expr, parse::to_a1},
    ops::{ColumnContent, TabContent},
    style::Style,
    value::{ErrorKind, Value},
};

use super::{
    formula::{Place, Prepared, TableDef, prepare},
    read_text_runs,
    styles::CellStyles,
};

/// What every sheet of a workbook reads against.
pub(super) struct Ctx<'a> {
    pub strings: &'a [Arc<str>],
    pub styles: &'a CellStyles,
    pub palette: &'a Palette,
    pub names: &'a IndexNames<'a>,
    pub tab_names: &'a [String],
    pub tables: &'a [TableDef],
    pub date1904: bool,
}

pub(super) struct SheetOut {
    pub content: TabContent,
    pub color: Option<String>,
    /// The relationship id of the drawing (charts, pictures).
    pub drawing: Option<String>,
}

/// Days between the 1904 and 1900 date systems.
const DAYS_1904: f64 = 1462.0;

/// Excel's column width (in characters of the default font) as pixels, for Calibri 11 (7-pixel digits).
pub(crate) fn col_width_px(chars: f64) -> f32 {
    if chars <= 0.0 {
        return 0.0;
    }
    (((256.0 * chars + (128.0f64 / 7.0).trunc()) / 256.0) * 7.0).trunc() as f32
}

/// Points as pixels at 96 dpi.
pub(crate) fn pt_px(pt: f64) -> f32 {
    (pt * 96.0 / 72.0).round() as f32
}

/// `B7` as (row 6, col 1), from the raw attribute bytes, without allocating.
fn parse_ref(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 0;
    let mut col: u32 = 0;
    while i < b.len() && b[i].is_ascii_alphabetic() {
        col = col.checked_mul(26)?.checked_add((b[i].to_ascii_uppercase() - b'A') as u32 + 1)?;
        i += 1;
    }
    if i == 0 || i == b.len() || col == 0 {
        return None;
    }
    let mut row: u32 = 0;
    for &d in &b[i..] {
        if !d.is_ascii_digit() {
            return None;
        }
        row = row.checked_mul(10)?.checked_add((d - b'0') as u32)?;
    }
    if row == 0 {
        return None;
    }
    Some((row - 1, col - 1))
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Number,
    Shared,
    Str,
    Inline,
    Bool,
    Error,
    Date,
}

#[derive(Default)]
struct FInfo {
    kind: Option<String>,
    reference: Option<String>,
    si: Option<u32>,
    text: String,
}

struct Master {
    expr: Option<Arc<Expr>>,
}

/// Builds the tab's columns as cells arrive.
struct Builder {
    columns: Vec<ColumnContent>,
    styles: Vec<Style>,
    by_style: HashMap<Style, u32>,
    by_xf: HashMap<u32, Option<u32>>,
    max_row: Option<u32>,
    max_col: Option<u32>,
}

impl Builder {
    fn column(&mut self, c: u32) -> &mut ColumnContent {
        if self.columns.len() <= c as usize {
            self.columns.resize_with(c as usize + 1, ColumnContent::default);
        }
        &mut self.columns[c as usize]
    }

    fn touch(&mut self, r: u32, c: u32) {
        self.max_row = Some(self.max_row.map_or(r, |m| m.max(r)));
        self.max_col = Some(self.max_col.map_or(c, |m| m.max(c)));
    }

    fn value(&mut self, r: u32, c: u32, v: Value) {
        if v.is_empty() {
            return;
        }
        self.touch(r, c);
        let col = self.column(c);
        let len = col.values.len() as u32;
        if len == r {
            col.values.push(v);
        } else if len < r {
            col.values.resize(r as usize, Value::Empty);
            col.values.push(v);
        } else {
            col.values[r as usize] = v;
        }
    }

    fn formula(&mut self, r: u32, c: u32, text: String) {
        self.touch(r, c);
        self.column(c).formulas.push((r, text));
    }

    fn style_id(&mut self, style: &Style) -> Option<u32> {
        if style.is_default() {
            return None;
        }
        if let Some(&i) = self.by_style.get(style) {
            return Some(i);
        }
        let i = self.styles.len() as u32;
        self.styles.push(style.clone());
        self.by_style.insert(style.clone(), i);
        Some(i)
    }

    fn style_of_xf(&mut self, xf: u32, styles: &CellStyles) -> Option<u32> {
        if let Some(&s) = self.by_xf.get(&xf) {
            return s;
        }
        let s = styles.style(xf).cloned().and_then(|st| self.style_id(&st));
        self.by_xf.insert(xf, s);
        s
    }

    fn style(&mut self, r: u32, c: u32, ix: Option<u32>) {
        if let Some(ix) = ix {
            self.touch(r, c);
            self.column(c).styles.push((r, ix));
        }
    }

    fn has_style(&self, r: u32, c: u32) -> bool {
        self.columns.get(c as usize).is_some_and(|col| col.styles.binary_search_by_key(&r, |s| s.0).is_ok())
    }
}

/// Reads a text-only element's content into `out` (cleared first), up to its end.
fn read_into<R: BufRead>(xr: &mut Reader<R>, buf: &mut Vec<u8>, out: &mut String) {
    out.clear();
    let mut depth = 1usize;
    loop {
        buf.clear();
        let ev = match xr.read_event_into(buf) {
            Ok(ev) => ev,
            Err(_) => return,
        };
        match &ev {
            Event::Start(_) => depth += 1,
            Event::End(_) => {
                depth -= 1;
                if depth == 0 {
                    return;
                }
            }
            Event::Eof => return,
            other => push_text(out, other),
        }
    }
}

pub(super) fn read(r: &mut dyn BufRead, sheet: usize, ctx: &Ctx<'_>, warnings: &mut Warnings) -> Result<SheetOut, FormatError> {
    let mut xr = reader(r);
    let mut buf = Vec::with_capacity(4096);
    let mut b2 = Vec::with_capacity(1024);
    let mut b3 = Vec::with_capacity(1024);
    let mut vtext = String::new();
    let mut b = Builder {
        columns: Vec::new(),
        styles: Vec::new(),
        by_style: HashMap::default(),
        by_xf: HashMap::default(),
        max_row: None,
        max_col: None,
    };
    let mut content = TabContent::default();
    let mut color = None;
    let mut drawing = None;
    let mut default_row_pt = 15.0f64;
    let mut default_col_px: Option<f32> = None;
    let mut explicit_widths: HashMap<u32, f32> = HashMap::default();
    let mut row_styles: Vec<(u32, u32)> = Vec::new();
    let mut col_styles: Vec<(u32, u32, u32)> = Vec::new();
    let mut masters: HashMap<u32, Master> = HashMap::default();
    let mut pending: Vec<(u32, u32, u32)> = Vec::new();
    // Multi-cell array formulas: their area (beyond the anchor) is the formula's spill, not values.
    let mut arrays: Vec<Rect> = Vec::new();
    let tab_name = ctx.tab_names.get(sheet).cloned().unwrap_or_default();
    let mut row: u32 = 0;
    let mut next_row: u32 = 0;
    let mut next_col: u32 = 0;
    let mut in_sheet_data = false;

    loop {
        buf.clear();
        let ev = match xr.read_event_into(&mut buf) {
            Ok(ev) => ev,
            Err(e) => return Err(FormatError::read("Excel", format!("the worksheet \"{tab_name}\" is damaged ({e})"))),
        };
        let (e, empty) = match &ev {
            Event::Start(e) => (e, false),
            Event::Empty(e) => (e, true),
            Event::End(e) => {
                if local(e.name().into_inner()) == b"sheetData" {
                    in_sheet_data = false;
                }
                continue;
            }
            Event::Eof => break,
            _ => continue,
        };
        match name_of(e) {
            b"sheetData" => in_sheet_data = !empty,
            b"row" if in_sheet_data => {
                row = attr_u32(e, b"r").and_then(|r| r.checked_sub(1)).unwrap_or(next_row);
                next_row = row + 1;
                next_col = 0;
                if row >= MAX_ROWS {
                    return Err(FormatError::read("Excel", format!("\"{tab_name}\" has more than {MAX_ROWS} rows")));
                }
                if attr(e, b"hidden").is_some_and(|v| truthy(&v)) {
                    content.hidden_rows.push(row);
                }
                if let Some(ht) = attr_f64(e, b"ht") {
                    let custom = attr(e, b"customHeight").is_some_and(|v| truthy(&v));
                    if custom || (ht - default_row_pt).abs() > 0.5 {
                        content.row_heights.push((row, pt_px(ht)));
                    }
                }
                if attr(e, b"customFormat").is_some_and(|v| truthy(&v))
                    && let Some(s) = attr_u32(e, b"s")
                    && s != 0
                {
                    row_styles.push((row, s));
                }
            }
            b"c" if in_sheet_data => {
                let (r, c) = match attr_raw(e, b"r").and_then(|v| parse_ref(&v)) {
                    Some((rr, cc)) => {
                        if rr != row {
                            // A cell whose row disagrees with its <row>: trust the cell.
                            row = rr;
                            next_row = rr + 1;
                        }
                        (rr, cc)
                    }
                    None => (row, next_col),
                };
                next_col = c + 1;
                if c >= MAX_COLS {
                    continue;
                }
                let xf = attr_u32(e, b"s").unwrap_or(0);
                let kind = match attr_raw(e, b"t").as_deref() {
                    Some(b"s") => Kind::Shared,
                    Some(b"str") => Kind::Str,
                    Some(b"inlineStr") => Kind::Inline,
                    Some(b"b") => Kind::Bool,
                    Some(b"e") => Kind::Error,
                    Some(b"d") => Kind::Date,
                    _ => Kind::Number,
                };
                let mut has_v = false;
                let mut f: Option<FInfo> = None;
                let mut inline: Option<String> = None;
                if !empty {
                    loop {
                        b2.clear();
                        let ev = match xr.read_event_into(&mut b2) {
                            Ok(ev) => ev,
                            Err(_) => break,
                        };
                        match &ev {
                            Event::Start(ce) => match name_of(ce) {
                                b"v" => {
                                    read_into(&mut xr, &mut b3, &mut vtext);
                                    has_v = true;
                                }
                                b"f" => {
                                    let mut fi = FInfo {
                                        kind: attr(ce, b"t"),
                                        reference: attr(ce, b"ref"),
                                        si: attr_u32(ce, b"si"),
                                        text: String::new(),
                                    };
                                    read_into(&mut xr, &mut b3, &mut fi.text);
                                    f = Some(fi);
                                }
                                b"is" => inline = Some(super::decode_escapes(&read_text_runs(&mut xr, &mut b3)).into_owned()),
                                _ => skip_element(&mut xr, &mut b3),
                            },
                            Event::Empty(ce) => {
                                if name_of(ce) == b"f" {
                                    f = Some(FInfo {
                                        kind: attr(ce, b"t"),
                                        reference: attr(ce, b"ref"),
                                        si: attr_u32(ce, b"si"),
                                        text: String::new(),
                                    });
                                }
                            }
                            Event::End(_) | Event::Eof => break,
                            _ => {}
                        }
                    }
                }
                let is_date_xf = ctx.styles.is_date(xf);
                let v: Option<&str> = has_v.then_some(vtext.as_str());
                // The value it holds (for a formula: the result it last had).
                let mut value = match (kind, v) {
                    (Kind::Shared, Some(s)) => s
                        .trim()
                        .parse::<usize>()
                        .ok()
                        .and_then(|i| ctx.strings.get(i))
                        .map(|s| Value::Text(s.clone()))
                        .unwrap_or_default(),
                    (Kind::Str, Some(s)) => Value::from(super::decode_escapes(s).as_ref()),
                    (Kind::Inline, _) => inline.take().map(Value::from).or_else(|| v.map(Value::from)).unwrap_or_default(),
                    (Kind::Bool, Some(s)) => Value::Bool(truthy(s)),
                    (Kind::Error, Some(s)) => match ErrorKind::parse(s.trim()) {
                        Some(e) => Value::Error(e),
                        None => Value::from(s.trim()),
                    },
                    (Kind::Date, Some(s)) => match parse_date_time(s.trim()) {
                        Some((serial, _)) => Value::Number(serial),
                        None => Value::from(s.trim()),
                    },
                    (Kind::Number, Some(s)) => {
                        let t = s.trim();
                        if t.is_empty() {
                            Value::Empty
                        } else {
                            match t.parse::<f64>() {
                                Ok(n) if n.is_finite() => Value::Number(if ctx.date1904 && is_date_xf { n + DAYS_1904 } else { n }),
                                _ => Value::from(t),
                            }
                        }
                    }
                    _ => Value::Empty,
                };
                // The style: a `t="d"` cell without a date format gets one, so it shows as a date.
                let style_ix = if kind == Kind::Date && !is_date_xf && matches!(value, Value::Number(_)) {
                    let mut st = ctx.styles.style(xf).cloned().unwrap_or_default();
                    let n = value.as_number().unwrap_or(0.0);
                    st.num_fmt = Some(if n.fract() == 0.0 { "yyyy-mm-dd" } else { "yyyy-mm-dd h:mm:ss" }.to_owned());
                    b.style_id(&st)
                } else {
                    b.style_of_xf(xf, ctx.styles)
                };
                b.style(r, c, style_ix);

                let in_array = arrays.iter().any(|a| a.contains(CellPos::new(r, c)) && (a.r1, a.c1) != (r, c));
                let at = Place { row: r, col: c, sheet, names: ctx.names, tab_names: ctx.tab_names, tables: ctx.tables };
                let example = |text: &str| format!("{} on {tab_name}: ={}", CellPos::new(r, c), text.trim().trim_start_matches('='));
                if let Some(fi) = f {
                    match fi.kind.as_deref() {
                        Some("shared") if !fi.text.trim().is_empty() => {
                            let p = prepare(&fi.text, &at, true);
                            let expr = match p {
                                Prepared::Formula { text, expr } => {
                                    b.formula(r, c, text);
                                    expr
                                }
                                Prepared::Constant(cv) => {
                                    value = cv;
                                    None
                                }
                                Prepared::Failed(k) => {
                                    warnings.count(k, || example(&fi.text));
                                    None
                                }
                            };
                            if let Some(si) = fi.si {
                                masters.insert(si, Master { expr });
                            }
                        }
                        Some("shared") => match fi.si.and_then(|si| masters.get(&si).map(|m| m.expr.clone())) {
                            Some(Some(expr)) => b.formula(r, c, to_a1(&expr, r, c, ctx.names)),
                            Some(None) => warnings.count("unreadable", String::new),
                            None => {
                                if let Some(si) = fi.si {
                                    pending.push((r, c, si));
                                }
                            }
                        },
                        Some("array") => match prepare(&fi.text, &at, false) {
                            Prepared::Formula { text, .. } => {
                                b.formula(r, c, text);
                                if let Some(area) = fi.reference.as_deref().and_then(|a| Rect::parse(a, MAX_ROWS, MAX_COLS))
                                    && (area.rows() > 1 || area.cols() > 1)
                                {
                                    arrays.push(area);
                                }
                            }
                            Prepared::Constant(cv) => value = cv,
                            Prepared::Failed(k) => warnings.count(k, || example(&fi.text)),
                        },
                        Some("dataTable") => warnings.count("data_table", String::new),
                        _ if !fi.text.trim().is_empty() => match prepare(&fi.text, &at, false) {
                            Prepared::Formula { text, .. } => b.formula(r, c, text),
                            Prepared::Constant(cv) => value = cv,
                            Prepared::Failed(k) => warnings.count(k, || example(&fi.text)),
                        },
                        _ => {}
                    }
                }
                if !in_array {
                    b.value(r, c, value);
                }
                // Array areas end; forget the ones above.
                if arrays.len() > 16 {
                    arrays.retain(|a| a.r2 >= r);
                }
            }
            b"col" => {
                let (Some(min), Some(max)) = (attr_u32(e, b"min"), attr_u32(e, b"max")) else { continue };
                if min == 0 {
                    continue;
                }
                let (lo, hi) = (min - 1, (max.max(min) - 1).min(MAX_COLS - 1));
                let hidden = attr(e, b"hidden").is_some_and(|v| truthy(&v));
                let width = attr_f64(e, b"width");
                let custom = attr(e, b"customWidth").is_some_and(|v| truthy(&v));
                let style = attr_u32(e, b"style").filter(|&s| s != 0);
                if hidden {
                    content.hidden_cols.extend(lo..=hi);
                }
                if let Some(px) = width.map(col_width_px).filter(|&px| px > 0.0 || custom) {
                    for c in lo..=hi {
                        explicit_widths.insert(c, px);
                    }
                }
                if let Some(s) = style {
                    col_styles.push((lo, hi, s));
                }
            }
            b"sheetFormatPr" => {
                if let Some(h) = attr_f64(e, b"defaultRowHeight") {
                    default_row_pt = h;
                }
                if let Some(w) = attr_f64(e, b"defaultColWidth") {
                    default_col_px = Some(col_width_px(w));
                } else if let Some(base) = attr_f64(e, b"baseColWidth") {
                    default_col_px = Some((base * 7.0 + 5.0).round() as f32);
                }
            }
            b"pane" => {
                let state = attr(e, b"state").unwrap_or_default();
                if state == "frozen" || state == "frozenSplit" {
                    content.frozen_cols = attr_f64(e, b"xSplit").unwrap_or(0.0).max(0.0) as u32;
                    content.frozen_rows = attr_f64(e, b"ySplit").unwrap_or(0.0).max(0.0) as u32;
                }
            }
            b"tabColor" => color = ctx.palette.hex(&ColorSpec::from_element(e)),
            b"mergeCell" => {
                if let Some(rect) = attr(e, b"ref").and_then(|a| Rect::parse(&a, MAX_ROWS, MAX_COLS))
                    && (rect.rows() > 1 || rect.cols() > 1)
                {
                    content.merges.push(rect);
                }
            }
            b"drawing" => drawing = attr(e, b"id"),
            b"hyperlink" => warnings.count("hyperlink", String::new),
            b"conditionalFormatting" => {
                warnings.count("conditional", String::new);
                if !empty {
                    skip_element(&mut xr, &mut b2);
                }
            }
            b"dataValidation" => {
                warnings.count("validation", String::new);
                if !empty {
                    skip_element(&mut xr, &mut b2);
                }
            }
            b"autoFilter" => {
                warnings.count("filter", String::new);
                if !empty {
                    skip_element(&mut xr, &mut b2);
                }
            }
            b"extLst" if !empty => skip_element(&mut xr, &mut b2),
            _ => {}
        }
    }

    // Shared formulas whose master came after them.
    for (r, c, si) in pending {
        match masters.get(&si).map(|m| m.expr.clone()) {
            Some(Some(expr)) => b.formula(r, c, to_a1(&expr, r, c, ctx.names)),
            _ => warnings.count("unreadable", String::new),
        }
    }

    // Row and column default styles that show on empty cells (fills and borders), within the used area.
    let (max_r, max_c) = (b.max_row.unwrap_or(0), b.max_col.unwrap_or(0));
    let shows = |s: &Style| s.fill.is_some() || s.border_top.is_some() || s.border_bottom.is_some() || s.border_left.is_some() || s.border_right.is_some();
    let mut budget: u64 = 2_000_000;
    for col in b.columns.iter_mut() {
        col.styles.sort_unstable_by_key(|s| s.0);
        col.styles.dedup_by_key(|s| s.0);
    }
    if b.max_row.is_some() {
        for &(r, xf) in &row_styles {
            let Some(style) = ctx.styles.style(xf).filter(|s| shows(s)).cloned() else { continue };
            let Some(ix) = b.style_id(&style) else { continue };
            for c in 0..=max_c {
                if budget == 0 {
                    break;
                }
                if !b.has_style(r, c) {
                    budget -= 1;
                    let col = b.column(c);
                    let at = col.styles.partition_point(|s| s.0 < r);
                    col.styles.insert(at, (r, ix));
                }
            }
        }
        for &(lo, hi, xf) in &col_styles {
            let Some(style) = ctx.styles.style(xf).filter(|s| shows(s)).cloned() else { continue };
            let Some(ix) = b.style_id(&style) else { continue };
            for c in lo..=hi.min(max_c) {
                for r in 0..=max_r {
                    if budget == 0 {
                        break;
                    }
                    if !b.has_style(r, c) {
                        budget -= 1;
                        let col = b.column(c);
                        let at = col.styles.partition_point(|s| s.0 < r);
                        col.styles.insert(at, (r, ix));
                    }
                }
            }
        }
    }

    // Column widths: explicit ones, and the sheet's default for the other used columns when it isn't Excel's.
    let standard = 64.0f32;
    let sheet_default = default_col_px.unwrap_or(standard);
    let mut widths: Vec<(u32, f32)> =
        explicit_widths.iter().filter(|&(_, px)| (px - standard).abs() >= 1.0).map(|(&c, &px)| (c, px)).collect();
    if (sheet_default - standard).abs() >= 1.0 && b.max_col.is_some() {
        for c in 0..=max_c {
            if !explicit_widths.contains_key(&c) {
                widths.push((c, sheet_default));
            }
        }
    }
    widths.sort_unstable_by_key(|w| w.0);

    let mut rows = b.max_row.map_or(0, |r| r + 1);
    let mut cols = b.max_col.map_or(0, |c| c + 1);
    for m in &content.merges {
        rows = rows.max(m.r2 + 1);
        cols = cols.max(m.c2 + 1);
    }
    rows = rows.max(content.frozen_rows).max(content.row_heights.last().map_or(0, |h| h.0 + 1));
    cols = cols.max(content.frozen_cols).max(widths.last().map_or(0, |w| w.0 + 1));
    // Hidden rows and columns far past the data (a common way to hide the unused grid) stay in range.
    let hidden_row_limit = rows.max(1).saturating_add(10_000).min(MAX_ROWS);
    content.hidden_rows.retain(|&r| r < hidden_row_limit);
    rows = rows.max(content.hidden_rows.iter().max().map_or(0, |r| r + 1));
    cols = cols.max(content.hidden_cols.iter().max().map_or(0, |c| c + 1));
    content.hidden_cols.sort_unstable();
    content.hidden_cols.dedup();
    content.hidden_rows.sort_unstable();
    content.hidden_rows.dedup();
    if b.columns.len() < cols as usize {
        b.columns.resize_with(cols as usize, ColumnContent::default);
    }
    for col in b.columns.iter_mut() {
        while col.values.last().is_some_and(Value::is_empty) {
            col.values.pop();
        }
    }
    content.rows = rows;
    content.cols = cols;
    content.columns = b.columns;
    content.styles = b.styles;
    content.col_widths = widths;
    Ok(SheetOut { content, color, drawing })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units() {
        assert_eq!(parse_ref(b"A1"), Some((0, 0)));
        assert_eq!(parse_ref(b"XFD1048576"), Some((1_048_575, 16_383)));
        assert_eq!(parse_ref(b"aa10"), Some((9, 26)));
        assert_eq!(parse_ref(b"A"), None);
        assert_eq!(parse_ref(b"10"), None);
        assert_eq!(col_width_px(9.140625), 64.0);
        // A stored width of 8.43 is narrower than the default: Excel stores 9.140625 (with padding) for 64 px.
        assert_eq!(col_width_px(8.43), 59.0);
        assert_eq!(col_width_px(20.7109375), 145.0);
        assert_eq!(pt_px(15.0), 20.0);
        assert_eq!(pt_px(30.0), 40.0);
    }
}
