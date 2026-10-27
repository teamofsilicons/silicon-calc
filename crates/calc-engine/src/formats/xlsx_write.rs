//! Writing Excel workbooks with rust_xlsxwriter: values, formulas with their last results (spilling formulas as
//! dynamic arrays), every style property, column widths, row heights, merges, frozen panes, hidden rows and
//! columns, tab colours, defined names and charts.

use ahash::{HashMap, HashSet};
use rust_xlsxwriter::{
    Chart as XlChart, ChartType, Color, Format as XlFormat, FormatAlign, FormatBorder, FormatPattern, FormatUnderline,
    Formula, Workbook as XlBook, Worksheet,
};

use crate::{
    addr::{CellPos, Rect, quote_tab, split_tab},
    formats::{ExportOptions, FormatError, colors::parse_hex, export_tabs},
    formula::{
        ast::{CellRef, Coord, Expr, RangeRef, TabId},
        parse::{TabNames, parse_formula, to_a1},
    },
    model::{Chart, Tab},
    numfmt::is_date_format,
    style::{BorderStyle, CellControl, HAlign, Style, VAlign},
    value::{ErrorKind, Value},
    workbook::Workbook,
};

const FORMAT: &str = "Excel";
/// Excel's grid.
pub(crate) const XL_ROWS: u32 = 1_048_576;
pub(crate) const XL_COLS: u32 = 16_384;
/// The longest text an Excel cell holds.
const XL_TEXT: usize = 32_767;

/// Tab names as Excel allows them (at most 31 characters, unique), by tab id.
struct XlNames {
    by_id: HashMap<TabId, String>,
}

impl TabNames for XlNames {
    fn tab_id(&self, name: &str) -> Option<TabId> {
        self.by_id.iter().find(|(_, n)| n.eq_ignore_ascii_case(name)).map(|(id, _)| *id)
    }

    fn tab_name(&self, id: TabId) -> Option<String> {
        self.by_id.get(&id).cloned()
    }
}

fn excel_names(wb: &Workbook) -> XlNames {
    let mut taken: Vec<String> = Vec::new();
    let mut by_id = HashMap::default();
    for t in &wb.tabs {
        let clean: String = t.name.chars().filter(|c| !matches!(c, '[' | ']' | ':' | '*' | '?' | '/' | '\\')).collect();
        let clean = clean.trim().trim_matches('\'').trim().to_owned();
        let base: String = if clean.is_empty() || clean.eq_ignore_ascii_case("history") { format!("Tab {}", t.id) } else { clean };
        let mut name: String = base.chars().take(31).collect();
        let mut n = 2;
        while taken.iter().any(|x| x.eq_ignore_ascii_case(&name)) {
            let suffix = format!(" ({n})");
            name = base.chars().take(31 - suffix.chars().count()).collect::<String>() + &suffix;
            n += 1;
        }
        taken.push(name.clone());
        by_id.insert(t.id, name);
    }
    XlNames { by_id }
}

fn xl_err(e: rust_xlsxwriter::XlsxError) -> FormatError {
    FormatError::write(FORMAT, e.to_string())
}

pub(super) fn to_format(s: &Style) -> XlFormat {
    let mut f = XlFormat::new();
    if s.bold {
        f = f.set_bold();
    }
    if s.italic {
        f = f.set_italic();
    }
    if s.underline {
        f = f.set_underline(FormatUnderline::Single);
    }
    if s.strike {
        f = f.set_font_strikethrough();
    }
    if let Some(size) = s.font_size {
        f = f.set_font_size(size as f64);
    }
    if let Some(name) = &s.font {
        f = f.set_font_name(name.as_str());
    }
    if let Some(rgb) = s.color.as_deref().and_then(parse_hex) {
        f = f.set_font_color(Color::RGB(rgb));
    }
    if let Some(rgb) = s.fill.as_deref().and_then(parse_hex) {
        f = f.set_pattern(FormatPattern::Solid).set_background_color(Color::RGB(rgb));
    }
    match s.h_align {
        HAlign::Left => f = f.set_align(FormatAlign::Left),
        HAlign::Center => f = f.set_align(FormatAlign::Center),
        HAlign::Right => f = f.set_align(FormatAlign::Right),
        HAlign::General => {}
    }
    match s.v_align {
        VAlign::Top => f = f.set_align(FormatAlign::Top),
        VAlign::Middle => f = f.set_align(FormatAlign::VerticalCenter),
        VAlign::Bottom => {}
    }
    if s.wrap {
        f = f.set_text_wrap();
    }
    if s.rotation != 0 { f = f.set_rotation(if s.rotation == 255 { 270 } else { s.rotation }); }
    if matches!(s.control, Some(CellControl::Checkbox { checked: None, .. })) { f = f.set_checkbox(); }
    if let Some(code) = &s.num_fmt {
        f = f.set_num_format(code.as_str());
    }
    let border = |b: BorderStyle| match b {
        BorderStyle::None => FormatBorder::None,
        BorderStyle::Thin => FormatBorder::Thin,
        BorderStyle::Medium => FormatBorder::Medium,
        BorderStyle::Thick => FormatBorder::Thick,
        BorderStyle::Dashed => FormatBorder::Dashed,
        BorderStyle::Dotted => FormatBorder::Dotted,
        BorderStyle::Double => FormatBorder::Double,
    };
    if let Some(b) = &s.border_top {
        f = f.set_border_top(border(b.style));
        if let Some(rgb) = b.color.as_deref().and_then(parse_hex) {
            f = f.set_border_top_color(Color::RGB(rgb));
        }
    }
    if let Some(b) = &s.border_bottom {
        f = f.set_border_bottom(border(b.style));
        if let Some(rgb) = b.color.as_deref().and_then(parse_hex) {
            f = f.set_border_bottom_color(Color::RGB(rgb));
        }
    }
    if let Some(b) = &s.border_left {
        f = f.set_border_left(border(b.style));
        if let Some(rgb) = b.color.as_deref().and_then(parse_hex) {
            f = f.set_border_left_color(Color::RGB(rgb));
        }
    }
    if let Some(b) = &s.border_right {
        f = f.set_border_right(border(b.style));
        if let Some(rgb) = b.color.as_deref().and_then(parse_hex) {
            f = f.set_border_right_color(Color::RGB(rgb));
        }
    }
    f
}

/// A formula tree as Excel writes it: `A1#` as `ANCHORARRAY(A1)`, Calc-only error literals as `#N/A`, and
/// references past Excel's last row cut to it.
fn for_excel(e: &Expr, row: u32) -> Expr {
    let max = XL_ROWS - 1;
    let clamp_range = |r: &RangeRef| -> Expr {
        let Some((a, b)) = r.rows else { return Expr::Range(*r) };
        let (ra, rb) = (a.resolve(row), b.resolve(row));
        match (ra, rb) {
            (Some(x), _) if x > max => Expr::Error(ErrorKind::Ref),
            (_, Some(y)) if y > max => Expr::Range(RangeRef { rows: Some((a, Coord::Abs(max))), ..*r }),
            _ => Expr::Range(*r),
        }
    };
    let cell_ok = |c: &CellRef| c.row.resolve(row).is_some_and(|r| r <= max);
    match e {
        Expr::Spill(c) => Expr::Call("ANCHORARRAY".into(), vec![Expr::Cell(*c)]),
        Expr::Cell(c) if !cell_ok(c) => Expr::Error(ErrorKind::Ref),
        Expr::Range(r) => clamp_range(r),
        Expr::Error(ErrorKind::Spill | ErrorKind::Calc | ErrorKind::Circ) => Expr::Error(ErrorKind::NA),
        Expr::Call(n, args) => Expr::Call(n.clone(), args.iter().map(|a| for_excel(a, row)).collect()),
        Expr::Unary(op, x) => Expr::Unary(*op, Box::new(for_excel(x, row))),
        Expr::Binary(op, a, b) => Expr::Binary(*op, Box::new(for_excel(a, row)), Box::new(for_excel(b, row))),
        Expr::Paren(x) => Expr::Paren(Box::new(for_excel(x, row))),
        Expr::Array(rows) => Expr::Array(rows.iter().map(|r| r.iter().map(|a| for_excel(a, row)).collect()).collect()),
        other => other.clone(),
    }
}

/// Whether a formula reads a tab that isn't being written.
fn reads_other_tabs(e: &Expr, written: &HashSet<TabId>) -> bool {
    let mut other = false;
    e.walk(&mut |x| {
        let t = match x {
            Expr::Cell(c) | Expr::Spill(c) => c.tab,
            Expr::Range(r) => r.tab,
            _ => None,
        };
        if t.is_some_and(|t| !written.contains(&t)) {
            other = true;
        }
    });
    other
}

/// A value as the cached result text of a formula.
fn result_text(v: &Value) -> String {
    match v {
        Value::Number(n) if n.is_finite() => {
            if n.fract() == 0.0 && n.abs() < 1e15 {
                format!("{}", *n as i64)
            } else {
                format!("{n:?}")
            }
        }
        Value::Number(_) => "#NUM!".to_owned(),
        Value::Text(s) => {
            // rust_xlsxwriter types results by their text; text that reads as a number would be written as one.
            if s.trim().parse::<f64>().is_ok() || s.as_ref() == "TRUE" || s.as_ref() == "FALSE" { String::new() } else { s.to_string() }
        }
        Value::Bool(b) => (if *b { "TRUE" } else { "FALSE" }).to_owned(),
        Value::Error(e) => e.as_str().to_owned(),
        Value::Empty => String::new(),
    }
}

/// The seven errors every Excel reads as error values.
fn classic_error(e: ErrorKind) -> bool {
    matches!(e, ErrorKind::Null | ErrorKind::Div0 | ErrorKind::Value | ErrorKind::Ref | ErrorKind::Name | ErrorKind::Num | ErrorKind::NA)
}

fn clip_text(s: &str) -> &str {
    if s.len() <= XL_TEXT {
        return s;
    }
    let mut end = 0;
    for (count, (i, c)) in s.char_indices().enumerate() {
        if count >= XL_TEXT {
            break;
        }
        end = i + c.len_utf8();
    }
    &s[..end]
}

struct Formats {
    by_style: HashMap<u32, XlFormat>,
    default: XlFormat,
}

impl Formats {
    fn get(&mut self, wb: &Workbook, style: u32) -> &XlFormat {
        if style == 0 {
            return &self.default;
        }
        self.by_style.entry(style).or_insert_with(|| to_format(wb.style_of(style)))
    }
}

fn write_value(ws: &mut Worksheet, r: u32, c: u16, v: &Value, f: &XlFormat) -> Result<(), FormatError> {
    match v {
        Value::Number(n) if n.is_finite() => ws.write_number_with_format(r, c, *n, f).map(|_| ()),
        Value::Number(_) => ws.write_formula_with_format(r, c, Formula::new("#NUM!").set_result("#NUM!"), f).map(|_| ()),
        Value::Text(s) => ws.write_string_with_format(r, c, clip_text(s), f).map(|_| ()),
        Value::Bool(b) => ws.write_boolean_with_format(r, c, *b, f).map(|_| ()),
        // Error values are written as a formula that is only the error, which reads back as the error.
        Value::Error(e) if classic_error(*e) => ws.write_formula_with_format(r, c, Formula::new(e.as_str()).set_result(e.as_str()), f).map(|_| ()),
        Value::Error(e) => ws.write_string_with_format(r, c, e.as_str(), f).map(|_| ()),
        Value::Empty => ws.write_blank(r, c, f).map(|_| ()),
    }
    .map_err(xl_err)
}

pub(crate) fn export(wb: &Workbook, opts: &ExportOptions) -> Result<Vec<u8>, FormatError> {
    let tabs = export_tabs(wb, opts, FORMAT)?;
    let written: HashSet<TabId> = tabs.iter().copied().collect();
    let names = excel_names(wb);
    for &t in &tabs {
        let tab = wb.tab(t).expect("listed");
        if let Some(used) = tab.used_rect()
            && (used.r2 >= XL_ROWS || used.c2 >= XL_COLS)
        {
            return Err(FormatError::write(
                FORMAT,
                format!("\"{}\" has {} rows, and Excel holds at most 1,048,576", tab.name, used.r2 + 1),
            ));
        }
    }
    let mut book = XlBook::new();
    let mut formats = Formats { by_style: HashMap::default(), default: XlFormat::new() };
    for &t in &tabs {
        let tab = wb.tab(t).expect("listed");
        let ws = book.add_worksheet();
        ws.set_name(names.tab_name(t).unwrap_or_else(|| format!("Tab {t}"))).map_err(xl_err)?;
        write_tab(wb, tab, ws, &mut formats, &names, &written, opts.values_only)?;
        super::conditional::write(wb, tab, ws)?;
    }
    for n in &wb.names {
        if n.tab.is_some_and(|t| !written.contains(&t)) {
            continue;
        }
        let text = n.text.trim_start_matches('=');
        let text = match parse_formula(text, 0, 0, &wb.names()) {
            Ok(e) if super::programs::uses_javascript(wb, &e) => continue,
            Ok(e) if reads_other_tabs(&e, &written) => continue,
            Ok(e) => to_a1(&for_excel(&super::programs::portable_expr(wb, &e, n.tab.unwrap_or(tabs[0]), 0, 0), 0), 0, 0, &names),
            Err(_) => text.to_owned(),
        };
        let full = match n.tab.and_then(|t| names.tab_name(t)) {
            Some(sheet) => format!("{}!{}", quote_tab(&sheet), n.name),
            None => n.name.clone(),
        };
        // A name Excel doesn't allow is left out rather than failing the download.
        let _ = book.define_name(full, &format!("={text}"));
    }
    let mut extras = Vec::new();
    for &t in &tabs {
        let tab = wb.tab(t).expect("listed");
        let mut cells = Vec::new();
        tab.for_each_occupied(|rid, cid, _, _| {
            let cell = tab.cell_by_id(rid, cid);
            let style = wb.style_of(cell.style);
            if (style.clip || style.control.is_some()) && let (Some(row), Some(col)) = (tab.rows.pos_of(rid), tab.cols.pos_of(cid)) {
                cells.push(super::cell_controls::CellExtra { row, col, clip: style.clip, control: style.control.clone() });
            }
        });
        if !cells.is_empty() { extras.push((names.tab_name(t).expect("tab name"), cells)); }
    }
    let bytes=super::cell_controls::append(book.save_to_buffer().map_err(xl_err)?, &extras)?;
    let rules=tabs.iter().filter_map(|t| {let tab=wb.tab(*t)?;if tab.format_rules.is_empty(){return None;}Some((names.tab_name(*t)?,wb.capture_tab(*t)?.format_rules))}).collect::<Vec<_>>();
    let bytes = super::conditional::append(bytes,&rules)?;
    let names=tabs.iter().map(|id|(*id,names.tab_name(*id).expect("tab name"))).collect::<Vec<_>>();
    let programs=super::programs::encode(wb,opts,&names)?;
    super::programs::append_package(bytes,programs.as_deref(),false)
}

fn write_tab(
    wb: &Workbook,
    tab: &Tab,
    ws: &mut Worksheet,
    formats: &mut Formats,
    names: &XlNames,
    written: &HashSet<TabId>,
    values_only: bool,
) -> Result<(), FormatError> {
    let pos = |rid: u32, cid: u32| -> Option<(u32, u32)> { Some((tab.rows.pos_of(rid)?, tab.cols.pos_of(cid)?)) };
    let fits = |r: u32, c: u32| r < XL_ROWS && c < XL_COLS;

    // Sizes, hidden rows and columns.
    for (&cid, &w) in &tab.col_widths {
        if let Some(c) = tab.cols.pos_of(cid).filter(|&c| c < XL_COLS) {
            ws.set_column_width_pixels(c as u16, w.round().max(0.0) as u32).map_err(xl_err)?;
        }
    }
    for (&rid, &h) in &tab.row_heights {
        if let Some(r) = tab.rows.pos_of(rid).filter(|&r| r < XL_ROWS) {
            ws.set_row_height_pixels(r, h.round().max(1.0) as u32).map_err(xl_err)?;
        }
    }
    for &rid in &tab.hidden_rows {
        if let Some(r) = tab.rows.pos_of(rid).filter(|&r| r < XL_ROWS) {
            ws.set_row_hidden(r).map_err(xl_err)?;
        }
    }
    for &cid in &tab.hidden_cols {
        if let Some(c) = tab.cols.pos_of(cid).filter(|&c| c < XL_COLS) {
            ws.set_column_hidden(c as u16).map_err(xl_err)?;
        }
    }
    if tab.frozen_rows > 0 || tab.frozen_cols > 0 {
        ws.set_freeze_panes(tab.frozen_rows.min(XL_ROWS - 1), tab.frozen_cols.min(XL_COLS - 1) as u16).map_err(xl_err)?;
    }
    if let Some(rgb) = tab.color.as_deref().and_then(parse_hex) {
        ws.set_tab_color(Color::RGB(rgb));
    }
    // Merges first: writing the anchor afterwards replaces the blank the merge leaves in it.
    for m in &tab.merges {
        let Some(r) = tab.pos_rect(m) else { continue };
        if !fits(r.r2, r.c2) || (r.rows() == 1 && r.cols() == 1) {
            continue;
        }
        let style = tab.cell_at(CellPos::new(r.r1, r.c1)).style;
        let f = formats.get(wb, style).clone();
        // Overlapping merges can't be written; the first one wins.
        let _ = ws.merge_range(r.r1, r.c1 as u16, r.r2, r.c2 as u16, "", &f);
    }

    let mut spilled: Vec<(u32, u32)> = Vec::new();
    for (rid, cid) in tab.occupied() {
        let Some((r, c)) = pos(rid, cid) else { continue };
        if !fits(r, c) {
            continue;
        }
        let cell = tab.cell_by_id(rid, cid);
        if cell.spilled {
            spilled.push((r, c));
            continue;
        }
        let f = formats.get(wb, cell.style).clone();
        if let Some(CellControl::Dropdown { options }) = &wb.style_of(cell.style).control {
            // Excel limits an inline list to 255 characters and uses commas as separators.
            // Longer/custom rules still round-trip through the Calc XML part above.
            if options.iter().all(|s| !s.contains(',')) && options.join(",").chars().count() <= 255 {
                let validation = rust_xlsxwriter::DataValidation::new().allow_list_strings(options).map_err(xl_err)?;
                ws.add_data_validation(r, c as u16, r, c as u16, &validation).map_err(xl_err)?;
            }
        }
        let group = cell.formula.and_then(|g| wb.group(g));
        match group {
            Some(g) if !values_only && !reads_other_tabs(&g.expr, written) && !super::programs::uses_javascript(wb,&g.expr) => {
                let portable=super::programs::portable_expr(wb,&g.expr,tab.id,r,c);
                let text = to_a1(&for_excel(&portable, r), r, c, names);
                let formula = Formula::new(&text).set_result(result_text(&cell.value));
                let spill = wb.spills.get(&(tab.id, rid, cid)).copied();
                let res = match spill {
                    Some(s) if !s.blocked && (s.rows > 1 || s.cols > 1) && fits(r + s.rows - 1, c + s.cols - 1) => {
                        ws.write_dynamic_array_formula_with_format(r, c as u16, r + s.rows - 1, (c + s.cols - 1) as u16, formula, &f)
                    }
                    Some(_) => ws.write_dynamic_formula_with_format(r, c as u16, formula, &f),
                    None => ws.write_formula_with_format(r, c as u16, formula, &f),
                };
                res.map_err(xl_err)?;
            }
            _ => write_value(ws, r, c as u16, &cell.value, &f)?,
        }
    }
    // A spill's cells after their anchors (an anchor's array formula fills its area with zeros first).
    for (r, c) in spilled {
        let cell = tab.cell_at(CellPos::new(r, c));
        let f = formats.get(wb, cell.style).clone();
        write_value(ws, r, c as u16, &cell.value, &f)?;
    }

    for chart in wb.charts.iter().filter(|ch| ch.tab == tab.id) {
        if let Some(xc) = build_chart(wb, chart, tab, names, written) {
            let (row, col) = (chart.anchor_row.min(XL_ROWS - 1), chart.anchor_col.min(XL_COLS - 1) as u16);
            ws.insert_chart(row, col, &xc).map_err(xl_err)?;
        }
    }
    Ok(())
}

/// The area a chart reads: its tab and rectangle.
pub(crate) fn chart_area<'a>(wb: &'a Workbook, chart: &Chart) -> Option<(&'a Tab, Rect)> {
    let (tab_name, rest) = split_tab(&chart.range);
    let tab = match tab_name {
        Some(n) => wb.tab_by_name(&n)?,
        None => wb.tab(chart.tab)?,
    };
    let mut rect = Rect::parse(rest, crate::addr::MAX_ROWS, crate::addr::MAX_COLS)?;
    // Whole columns stop at the data.
    if let Some(used) = tab.used_rect() {
        rect.r2 = rect.r2.min(used.r2.max(rect.r1));
        rect.c2 = rect.c2.min(used.c2.max(rect.c1));
    }
    Some((tab, rect))
}

/// How a chart's range splits into series: the first row holds series names and the first column categories
/// (or x values for scatter charts) when they are labels.
pub(crate) struct ChartLayout {
    pub header: bool,
    pub categories: bool,
}

pub(crate) fn chart_layout(wb: &Workbook, tab: &Tab, rect: &Rect, kind: &str) -> ChartLayout {
    let is_label = |r: u32, c: u32| -> bool {
        let cell = tab.cell_at(CellPos::new(r, c));
        match cell.value {
            Value::Text(_) => true,
            Value::Number(_) => wb.style_of(cell.style).num_fmt.as_deref().is_some_and(is_date_format),
            _ => false,
        }
    };
    let header = rect.rows() > 1
        && (rect.c1..=rect.c2).any(|c| matches!(tab.cell_at(CellPos::new(rect.r1, c)).value, Value::Text(_)))
        && (rect.c1..=rect.c2).all(|c| !matches!(tab.cell_at(CellPos::new(rect.r1, c)).value, Value::Number(_)) || c == rect.c1);
    let first_data = rect.r1 + header as u32;
    let categories = rect.cols() > 1
        && (kind == "scatter" || (first_data..=rect.r2).take(50).any(|r| is_label(r, rect.c1)));
    ChartLayout { header, categories }
}

fn build_chart(wb: &Workbook, chart: &Chart, own: &Tab, names: &XlNames, written: &HashSet<TabId>) -> Option<XlChart> {
    let (data_tab, rect) = chart_area(wb, chart)?;
    if !written.contains(&data_tab.id) {
        return None;
    }
    let sheet = names.tab_name(data_tab.id)?;
    let layout = chart_layout(wb, data_tab, &rect, &chart.kind);
    let first_data = rect.r1 + layout.header as u32;
    if first_data > rect.r2 {
        return None;
    }
    let first_series = rect.c1 + layout.categories as u32;
    if first_series > rect.c2 {
        return None;
    }
    let kind = match chart.kind.as_str() {
        "line" => ChartType::Line,
        "bar" => ChartType::Bar,
        "area" => ChartType::Area,
        "pie" => ChartType::Pie,
        "scatter" => ChartType::Scatter,
        _ => ChartType::Column,
    };
    let mut xc = XlChart::new(kind);
    let (r1, r2) = (first_data.min(XL_ROWS - 1), rect.r2.min(XL_ROWS - 1));
    for c in first_series..=rect.c2.min(XL_COLS - 1) {
        let s = xc.add_series();
        s.set_values((sheet.as_str(), r1, c as u16, r2, c as u16));
        if layout.categories {
            s.set_categories((sheet.as_str(), r1, rect.c1 as u16, r2, rect.c1 as u16));
        }
        if layout.header {
            s.set_name((sheet.as_str(), rect.r1, c as u16));
        }
        if kind == ChartType::Pie {
            break;
        }
    }
    if !chart.title.trim().is_empty() {
        xc.title().set_name(chart.title.trim());
    }
    xc.set_width(chart.width.max(50));
    xc.set_height(chart.height.max(50));
    let _ = own;
    Some(xc)
}
