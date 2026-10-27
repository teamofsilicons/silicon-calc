//! The edits of `POST {line}/edits` (docs/API.md, Editing), turned into engine operations.
//!
//! A request is read and checked first (addresses, ranges, counts, the accounts protections name), then each edit
//! becomes a stage that builds its operation inside the line's task from the workbook as the earlier edits left it,
//! so ids for new rows and columns are given out in order. Cells that have no ids yet get them through an `Extend`
//! in the same operation.

use axum::http::StatusCode;
use calc_engine::{
    Op, Rect, StylePatch, Tab, Workbook,
    addr::{MAX_COLS, MAX_ROWS, col_index},
    formula::parse::parse_formula,
    model::{Axis, Chart, DefinedName, IdRect, Protection},
    ops::{CellWrite, ClearWhat, TabProps},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    task::Stage,
    view::{self, TabRef, Window, find_tab},
};
use crate::{
    config::MAX_CELLS_PER_EDIT,
    error::{ApiError, ApiResult, json_problem},
    state::AppState,
};

/// The most edits one request may hold.
const MAX_EDITS: usize = 10_000;
const CHART_KINDS: &[&str] = &["line", "bar", "column", "area", "pie", "scatter"];

#[derive(Deserialize)]
struct CellInput {
    a: String,
    #[serde(default)]
    input: Value,
}

#[derive(Deserialize)]
struct SortKey {
    col: String,
    #[serde(default = "yes")]
    asc: bool,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
struct NameIn {
    name: String,
    #[serde(rename = "ref")]
    reference: String,
}

#[derive(Deserialize)]
struct ChartIn {
    id: Option<String>,
    tab: TabRef,
    kind: String,
    #[serde(default)]
    title: String,
    range: String,
    anchor: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(Deserialize)]
struct ProtectionIn {
    id: Option<String>,
    tab: TabRef,
    range: Option<String>,
    #[serde(default)]
    editors: Vec<String>,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Edit {
    Set {
        tab: TabRef,
        cells: Option<Vec<CellInput>>,
        at: Option<String>,
        rows: Option<Vec<Vec<Value>>>,
    },
    Values {
        tab: TabRef,
        at: String,
        rows: Vec<Vec<Value>>,
    },
    Style {
        tab: TabRef,
        range: String,
        #[serde(default)]
        patch: StylePatch,
    },
    Clear {
        tab: TabRef,
        range: String,
        what: Option<ClearWhat>,
    },
    InsertRows {
        tab: TabRef,
        at: u32,
        #[serde(default = "one")]
        count: u32,
    },
    InsertCols {
        tab: TabRef,
        at: u32,
        #[serde(default = "one")]
        count: u32,
    },
    DeleteRows {
        tab: TabRef,
        at: u32,
        #[serde(default = "one")]
        count: u32,
    },
    DeleteCols {
        tab: TabRef,
        at: u32,
        #[serde(default = "one")]
        count: u32,
    },
    MoveRows {
        tab: TabRef,
        from: u32,
        #[serde(default = "one")]
        count: u32,
        to: u32,
    },
    MoveCols {
        tab: TabRef,
        from: u32,
        #[serde(default = "one")]
        count: u32,
        to: u32,
    },
    Sort {
        tab: TabRef,
        range: String,
        by: Vec<SortKey>,
        #[serde(default)]
        header: bool,
    },
    Fill {
        tab: TabRef,
        from: String,
        to: String,
    },
    AddTab {
        name: String,
        index: Option<usize>,
    },
    RenameTab {
        tab: TabRef,
        name: String,
    },
    MoveTab {
        tab: TabRef,
        index: usize,
    },
    DeleteTab {
        tab: TabRef,
    },
    TabProps {
        tab: TabRef,
        frozen_rows: Option<u32>,
        frozen_cols: Option<u32>,
        #[serde(default, deserialize_with = "crate::sheets::explicit_null")]
        color: Option<Option<String>>,
    },
    ColWidth {
        tab: TabRef,
        cols: String,
        width: Option<f64>,
    },
    RowHeight {
        tab: TabRef,
        rows: String,
        height: Option<f64>,
    },
    HideRows {
        tab: TabRef,
        range: String,
    },
    ShowRows {
        tab: TabRef,
        range: String,
    },
    HideCols {
        tab: TabRef,
        range: String,
    },
    ShowCols {
        tab: TabRef,
        range: String,
    },
    Merge {
        tab: TabRef,
        range: String,
    },
    Unmerge {
        tab: TabRef,
        range: String,
    },
    FormatRule { tab: TabRef, range: String, id: Option<String>, rule: calc_engine::conditional::Kind },
    DeleteFormatRule { tab: TabRef, id: String },
    Names {
        names: Vec<NameIn>,
    },
    Chart {
        chart: ChartIn,
    },
    DeleteChart {
        id: String,
    },
    Protect {
        protection: ProtectionIn,
    },
    Unprotect {
        id: String,
    },
    Undo,
    Redo,
}

fn one() -> u32 {
    1
}

/// One edit, ready for the line's task.
pub(crate) enum Prepared {
    Stage(Stage),
    Undo,
    Redo,
}

/// A whole request, read and checked.
pub(crate) struct Parsed {
    pub base_version: Option<i64>,
    pub client_id: Option<String>,
    pub items: Vec<Prepared>,
    /// The first edit only the sheet's owner may make (protections), if any.
    pub owner_only: Option<usize>,
}

#[derive(Deserialize)]
struct Body {
    base_version: Option<i64>,
    #[serde(default)]
    edits: Vec<Value>,
    client_id: Option<String>,
}

/// Adds `details.index` to an error about one edit.
fn at_index(mut e: ApiError, index: usize) -> ApiError {
    let mut details = match e.details.take() {
        Some(Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    details.insert("index".into(), json!(index));
    e.details = Some(Value::Object(details));
    if !e.message.starts_with("Edit ") {
        e.message = format!("Edit {index}: {}", e.message);
    }
    e
}

fn bad(field: &str, message: impl Into<String>) -> ApiError {
    ApiError::invalid_field(field, message)
}

fn not_found(code: &'static str, message: String, hint: &str) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, code, message, hint)
}

fn too_many_cells(cells: u64) -> ApiError {
    ApiError::too_large(
        format!("This edit covers {cells} cells; one request can change at most {MAX_CELLS_PER_EDIT}."),
        "Split it into several smaller requests, or bring big data in as a file (calc import).",
    )
}

/// What one typed input becomes as text: strings as they are, numbers and logicals spelled out, null empty.
fn input_text(v: &Value) -> Result<String, String> {
    Ok(match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => (if *b { "TRUE" } else { "FALSE" }).to_owned(),
        Value::Number(n) => n.to_string(),
        other => return Err(format!("{other} is not something to type into a cell; send text like \"12\" or \"=A1*2\".")),
    })
}

/// A raw value for `values` edits: numbers, strings, logicals, null, or a Value object (`{"t": "error", "v": "#N/A"}`).
fn raw_value(v: &Value) -> Result<calc_engine::Value, String> {
    Ok(match v {
        Value::Null => calc_engine::Value::Empty,
        Value::Bool(b) => calc_engine::Value::Bool(*b),
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.is_finite() => calc_engine::Value::Number(f),
            _ => return Err(format!("{n} is not a number a cell can hold.")),
        },
        Value::String(s) => calc_engine::Value::from(s.as_str()),
        Value::Object(_) => serde_json::from_value(v.clone())
            .map_err(|_| format!("{v} is not a value; values look like {{\"t\": \"number\", \"v\": 1.5}}."))?,
        Value::Array(_) => return Err("A cell can't hold a list.".to_owned()),
    })
}

/// The id a position on an axis has, or will have once the axis is extended to it.
pub(crate) fn axis_id(axis: &Axis, pos: u32) -> u32 {
    axis.id_at(pos).unwrap_or_else(|| axis.next_id() + (pos - axis.len()))
}

/// `Extend` when positions up to `rows` × `cols` have no ids yet.
pub(crate) fn extend_for(t: &Tab, rows: u32, cols: u32) -> Option<Op> {
    (rows > t.rows.len() || cols > t.cols.len())
        .then(|| Op::Extend { tab: t.id, rows: rows.max(t.rows.len()), cols: cols.max(t.cols.len()) })
}

pub(crate) fn with_extend(ext: Option<Op>, op: Op) -> Op {
    match ext {
        Some(e) => Op::Batch { ops: vec![e, op] },
        None => op,
    }
}

/// An area by ids (given out as needed), with the `Extend` it needs.
pub(crate) fn id_area(t: &Tab, r: Rect) -> (IdRect, Option<Op>) {
    let area = IdRect {
        r1: axis_id(&t.rows, r.r1),
        c1: axis_id(&t.cols, r.c1),
        r2: axis_id(&t.rows, r.r2),
        c2: axis_id(&t.cols, r.c2),
    };
    (area, extend_for(t, r.r2 + 1, r.c2 + 1))
}

/// A range on a tab: open axes (whole columns or rows) stop at the larger of the tab's used area and the rows or
/// columns it has.
fn bounded(t: &Tab, w: Window) -> Rect {
    let mut r = w.rect;
    let used = t.used_rect();
    if w.open_rows {
        let last = t.rows.len().max(used.map_or(0, |u| u.r2 + 1)).max(r.r1 + 1) - 1;
        r.r2 = last.min(MAX_ROWS - 1);
    }
    if w.open_cols {
        let last = t.cols.len().max(used.map_or(0, |u| u.c2 + 1)).max(r.c1 + 1) - 1;
        r.c2 = last.min(MAX_COLS - 1);
    }
    r
}

fn cells_of(r: &Rect) -> u64 {
    r.rows() as u64 * r.cols() as u64
}

fn check_cells(r: &Rect) -> ApiResult<()> {
    if cells_of(r) > MAX_CELLS_PER_EDIT as u64 { Err(too_many_cells(cells_of(r))) } else { Ok(()) }
}

/// `#1F5FB8`, `#abc`: a colour, as `#RRGGBB`.
fn color(text: &str) -> ApiResult<String> {
    let t = text.trim().trim_start_matches('#');
    let hex = match t.len() {
        3 => t.chars().flat_map(|c| [c, c]).collect::<String>(),
        6 => t.to_owned(),
        _ => String::new(),
    };
    if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(format!("#{}", hex.to_ascii_uppercase()))
    } else {
        Err(bad("color", format!("\"{text}\" is not a colour; write it like #1F5FB8.")))
    }
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else { return false };
    (first.is_alphabetic() || first == '_' || first == '\\')
        && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        && name.chars().count() <= 255
        && calc_engine::CellPos::parse(name).is_none()
        && !name.eq_ignore_ascii_case("TRUE")
        && !name.eq_ignore_ascii_case("FALSE")
}

/// A short random id for charts and protections.
fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..12].to_owned()
}

fn tab_of<'a>(wb: &'a Workbook, r: &TabRef) -> ApiResult<&'a Tab> {
    find_tab(wb, r)
}

/// Reads and checks a request body; each edit becomes a stage (or an undo/redo).
pub(crate) async fn parse(state: &AppState, body: Value) -> ApiResult<Parsed> {
    let body: Body = serde_json::from_value(body).map_err(|e| json_problem(&e))?;
    if body.edits.is_empty() {
        return Err(bad("edits", "Give at least one edit, like {\"type\": \"set\", \"tab\": 1, \"cells\": [{\"a\": \"B2\", \"input\": \"5\"}]}."));
    }
    if body.edits.len() > MAX_EDITS {
        return Err(ApiError::too_large(
            format!("This request holds {} edits; one request can hold at most {MAX_EDITS}.", body.edits.len()),
            "Send them in several requests.",
        ));
    }
    if let Some(b) = body.base_version
        && b < 0
    {
        return Err(bad("base_version", "base_version is a version number: 0 or more."));
    }
    let client_id = body.client_id.map(|c| c.trim().chars().take(200).collect::<String>()).filter(|c| !c.is_empty());
    let mut items = Vec::with_capacity(body.edits.len());
    let mut owner_only = None;
    let mut cells: u64 = 0;
    let n = body.edits.len();
    for (i, raw) in body.edits.into_iter().enumerate() {
        let edit: Edit = serde_json::from_value(raw).map_err(|e| {
            let mut err = json_problem(&e);
            if err.message.contains("unknown variant") {
                err = bad("type", format!("This edit's type isn't one Calc knows ({}).", e.to_string().split(" at line ").next().unwrap_or("")))
                    .with_hint("Edit types are set, values, style, clear, insert_rows, delete_rows, move_rows, sort, fill, add_tab, rename_tab, move_tab, delete_tab, tab_props, col_width, row_height, hide_rows, show_rows, merge, unmerge, names, chart, delete_chart, protect, unprotect, undo and redo (see docs/API.md).");
            }
            at_index(err, i)
        })?;
        if matches!(edit, Edit::Protect { .. } | Edit::Unprotect { .. }) && owner_only.is_none() {
            owner_only = Some(i);
        }
        let item = prepare(state, edit, &mut cells).await.map_err(|e| at_index(e, i))?;
        if matches!(item, Prepared::Undo | Prepared::Redo) && n > 1 {
            return Err(at_index(bad("type", "Undo and redo go alone in a request."), i));
        }
        items.push(item);
    }
    Ok(Parsed { base_version: body.base_version, client_id, items, owner_only })
}

fn count(cells: &mut u64, n: u64) -> ApiResult<()> {
    *cells += n;
    if *cells > MAX_CELLS_PER_EDIT as u64 { Err(too_many_cells(*cells)) } else { Ok(()) }
}

/// Turns one edit into its stage.
async fn prepare(state: &AppState, edit: Edit, cells: &mut u64) -> ApiResult<Prepared> {
    Ok(Prepared::Stage(match edit {
        Edit::Undo => return Ok(Prepared::Undo),
        Edit::Redo => return Ok(Prepared::Redo),
        Edit::Set { tab, cells: list, at, rows } => {
            let mut writes: Vec<(u32, u32, String)> = Vec::new();
            match (list, at, rows) {
                (Some(list), None, None) => {
                    count(cells, list.len() as u64)?;
                    for (k, c) in list.into_iter().enumerate() {
                        let p = view::parse_cell(&c.a, &format!("cells[{k}].a"))?;
                        let text = input_text(&c.input).map_err(|m| bad(&format!("cells[{k}].input"), m))?;
                        writes.push((p.row, p.col, text));
                    }
                }
                (None, Some(at), Some(rows)) => {
                    let p = view::parse_cell(&at, "at")?;
                    count(cells, rows.iter().map(|r| r.len() as u64).sum())?;
                    for (i, row) in rows.iter().enumerate() {
                        for (j, v) in row.iter().enumerate() {
                            let text = input_text(v).map_err(|m| bad(&format!("rows[{i}][{j}]"), m))?;
                            writes.push((p.row + i as u32, p.col + j as u32, text));
                        }
                    }
                }
                _ => {
                    return Err(bad(
                        "cells",
                        "A set edit gives either \"cells\" ([{\"a\": \"B2\", \"input\": \"5\"}]) or \"at\" and \"rows\" (a block of inputs).",
                    ));
                }
            }
            check_grid(&writes)?;
            Box::new(move |wb: &Workbook| {
                let tab = tab_of(wb, &tab)?.id;
                let w = writes
                    .into_iter()
                    .map(|(r, c, input)| (r, c, CellWrite { input: Some(input), ..Default::default() }))
                    .collect();
                Ok(Some(wb.prepare_writes(tab, w)?))
            })
        }
        Edit::Values { tab, at, rows } => {
            let p = view::parse_cell(&at, "at")?;
            count(cells, rows.iter().map(|r| r.len() as u64).sum())?;
            let mut writes: Vec<(u32, u32, calc_engine::Value)> = Vec::new();
            for (i, row) in rows.iter().enumerate() {
                for (j, v) in row.iter().enumerate() {
                    let value = raw_value(v).map_err(|m| bad(&format!("rows[{i}][{j}]"), m))?;
                    writes.push((p.row + i as u32, p.col + j as u32, value));
                }
            }
            check_grid(&writes)?;
            Box::new(move |wb: &Workbook| {
                let tab = tab_of(wb, &tab)?.id;
                let w = writes
                    .into_iter()
                    .map(|(r, c, v)| (r, c, CellWrite { clear: true, value: Some(v), ..Default::default() }))
                    .collect();
                Ok(Some(wb.prepare_writes(tab, w)?))
            })
        }
        Edit::Style { tab, range, patch } => {
            patch.validate().map_err(|message| ApiError::invalid_field("patch", message))?;
            let w = view::parse_range(&range, "range")?;
            Box::new(move |wb: &Workbook| {
                let t = tab_of(wb, &tab)?;
                let rect = bounded(t, w);
                check_cells(&rect)?;
                let (area, ext) = id_area(t, rect);
                Ok(Some(with_extend(ext, Op::StyleArea { tab: t.id, area, patch })))
            })
        }
        Edit::Clear { tab, range, what } => {
            let w = view::parse_range(&range, "range")?;
            Box::new(move |wb: &Workbook| {
                let t = tab_of(wb, &tab)?;
                let rect = bounded(t, w);
                if t.rows.is_empty() || t.cols.is_empty() || rect.r1 >= t.rows.len() || rect.c1 >= t.cols.len() {
                    return Ok(None);
                }
                let clipped = Rect::new(rect.r1, rect.c1, rect.r2.min(t.rows.len() - 1), rect.c2.min(t.cols.len() - 1));
                check_cells(&clipped)?;
                Ok(Some(wb.prepare_clear(t.id, clipped, what.unwrap_or(ClearWhat::Contents))?))
            })
        }
        Edit::InsertRows { tab, at, count } => axis_stage(tab, true, AxisEdit::Insert, at, count)?,
        Edit::InsertCols { tab, at, count } => axis_stage(tab, false, AxisEdit::Insert, at, count)?,
        Edit::DeleteRows { tab, at, count } => axis_stage(tab, true, AxisEdit::Delete, at, count)?,
        Edit::DeleteCols { tab, at, count } => axis_stage(tab, false, AxisEdit::Delete, at, count)?,
        Edit::MoveRows { tab, from, count, to } => axis_stage(tab, true, AxisEdit::Move(to), from, count)?,
        Edit::MoveCols { tab, from, count, to } => axis_stage(tab, false, AxisEdit::Move(to), from, count)?,
        Edit::HideRows { tab, range } => hide_stage(tab, true, true, &range)?,
        Edit::ShowRows { tab, range } => hide_stage(tab, true, false, &range)?,
        Edit::HideCols { tab, range } => hide_stage(tab, false, true, &range)?,
        Edit::ShowCols { tab, range } => hide_stage(tab, false, false, &range)?,

        Edit::Sort { tab, range, by, header } => {
            let w = view::parse_range(&range, "range")?;
            if by.is_empty() {
                return Err(bad("by", "Say which columns to sort by, like [{\"col\": \"B\", \"asc\": false}]."));
            }
            let mut keys = Vec::new();
            for (k, key) in by.iter().enumerate() {
                let c = col_index(key.col.trim())
                    .ok_or_else(|| bad(&format!("by[{k}].col"), format!("\"{}\" is not a column; columns are letters like B.", key.col)))?;
                if c < w.rect.c1 || c > w.rect.c2 {
                    return Err(bad(&format!("by[{k}].col"), format!("Column {} is outside the range {range}.", key.col.trim())));
                }
                keys.push((c, key.asc));
            }
            Box::new(move |wb: &Workbook| {
                let t = tab_of(wb, &tab)?;
                let rect = bounded(t, w);
                Ok(Some(wb.prepare_sort(t.id, rect, &keys, header)?))
            })
        }
        Edit::Fill { tab, from, to } => {
            let src = view::parse_range(&from, "from")?;
            let dst = view::parse_range(&to, "to")?;
            if src.open_rows || src.open_cols {
                return Err(bad("from", "Fill from a cell or a bounded range, like C2 or C2:D2."));
            }
            Box::new(move |wb: &Workbook| {
                let t = tab_of(wb, &tab)?;
                let dst = bounded(t, dst);
                check_cells(&dst)?;
                Ok(Some(wb.prepare_fill(t.id, src.rect, dst)?))
            })
        }
        Edit::AddTab { name, index } => {
            let name = name.trim().to_owned();
            Box::new(move |wb: &Workbook| Ok(Some(wb.prepare_add_tab(&name, index))))
        }
        Edit::RenameTab { tab, name } => {
            let name = name.trim().to_owned();
            Box::new(move |wb: &Workbook| Ok(Some(Op::RenameTab { tab: tab_of(wb, &tab)?.id, name })))
        }
        Edit::MoveTab { tab, index } => Box::new(move |wb: &Workbook| Ok(Some(Op::MoveTab { tab: tab_of(wb, &tab)?.id, index }))),
        Edit::DeleteTab { tab } => Box::new(move |wb: &Workbook| Ok(Some(Op::DeleteTab { tab: tab_of(wb, &tab)?.id }))),
        Edit::TabProps { tab, frozen_rows, frozen_cols, color: c } => {
            if frozen_rows.is_none() && frozen_cols.is_none() && c.is_none() {
                return Err(bad("frozen_rows", "Give frozen_rows, frozen_cols or color."));
            }
            if frozen_rows.is_some_and(|f| f > 10_000) || frozen_cols.is_some_and(|f| f > 1_000) {
                return Err(bad("frozen_rows", "Freeze at most 10,000 rows and 1,000 columns."));
            }
            let color = match c {
                None => None,
                Some(None) => Some(None),
                Some(Some(text)) => Some(Some(color(&text)?)),
            };
            let props = TabProps { frozen_rows, frozen_cols, color, code: None, ..Default::default() };
            Box::new(move |wb: &Workbook| Ok(Some(Op::TabProps { tab: tab_of(wb, &tab)?.id, props })))
        }
        Edit::ColWidth { tab, cols, width } => {
            let (c1, c2) = view::parse_cols(&cols, "cols")?;
            let width = size(width, "width", 2_000.0)?;
            Box::new(move |wb: &Workbook| {
                let t = tab_of(wb, &tab)?;
                let widths = (c1..=c2).map(|p| (axis_id(&t.cols, p), width)).collect();
                Ok(Some(with_extend(extend_for(t, 0, c2 + 1), Op::ColWidths { tab: t.id, widths })))
            })
        }
        Edit::RowHeight { tab, rows, height } => {
            let (r1, r2) = view::parse_rows(&rows, "rows")?;
            if (r2 - r1) as usize >= MAX_CELLS_PER_EDIT {
                return Err(too_many_cells((r2 - r1 + 1) as u64));
            }
            let height = size(height, "height", 2_000.0)?;
            Box::new(move |wb: &Workbook| {
                let t = tab_of(wb, &tab)?;
                let heights = (r1..=r2).map(|p| (axis_id(&t.rows, p), height)).collect();
                Ok(Some(with_extend(extend_for(t, r2 + 1, 0), Op::RowHeights { tab: t.id, heights })))
            })
        }
        Edit::Merge { tab, range } => {
            let w = view::parse_range(&range, "range")?;
            Box::new(move |wb: &Workbook| {
                let t = tab_of(wb, &tab)?;
                let rect = bounded(t, w);
                if rect.rows() == 1 && rect.cols() == 1 {
                    return Err(bad("range", "Merging needs at least two cells, like A1:C1."));
                }
                check_cells(&rect)?;
                let (area, ext) = id_area(t, rect);
                Ok(Some(with_extend(ext, Op::Merge { tab: t.id, area })))
            })
        }
        Edit::Unmerge { tab, range } => {
            let w = view::parse_range(&range, "range")?;
            Box::new(move |wb: &Workbook| {
                let t = tab_of(wb, &tab)?;
                let rect = bounded(t, w);
                // Every merged area the range touches comes apart.
                let ops: Vec<Op> = t
                    .merges
                    .iter()
                    .filter(|m| t.pos_rect(m).is_some_and(|p| p.intersects(&rect)))
                    .map(|m| Op::Unmerge { tab: t.id, area: m.clone() })
                    .collect();
                Ok(match ops.len() {
                    0 => None,
                    1 => ops.into_iter().next(),
                    _ => Some(Op::Batch { ops }),
                })
            })
        }
        Edit::FormatRule { tab, range, id, rule } => {
            rule.validate().map_err(|e|bad("rule",e))?;
            let window=view::parse_range(&range,"range")?;
            let id=id.unwrap_or_else(||uuid::Uuid::new_v4().to_string());
            if id.len()>100 {return Err(bad("id","Use a rule id up to 100 characters."));}
            Box::new(move |wb:&Workbook| {
                let t=tab_of(wb,&tab)?;let rect=bounded(t,window);check_cells(&rect)?;
                if let calc_engine::conditional::Kind::Condition{condition,value,..}=&rule {
                    if condition=="formula" {parse_formula(value.trim().trim_start_matches('='),rect.r1,rect.c1,&wb.names()).map_err(|e|bad("value",e.message))?;}
                }
                let (area,ext)=id_area(t,rect);
                let mut rules=t.format_rules.clone();
                let pos=rules.iter().position(|r|r.id==id).unwrap_or(rules.len());rules.retain(|r|r.id!=id);
                rules.insert(pos,calc_engine::conditional::Rule{id,area,kind:rule});
                Ok(Some(with_extend(ext,Op::TabProps{tab:t.id,props:TabProps{format_rules:Some(rules),..Default::default()}})))
            })
        }
        Edit::DeleteFormatRule {tab,id} => Box::new(move |wb:&Workbook| {
            let t=tab_of(wb,&tab)?;let mut rules=t.format_rules.clone();rules.retain(|r|r.id!=id);
            Ok(Some(Op::TabProps{tab:t.id,props:TabProps{format_rules:Some(rules),..Default::default()}}))
        }),
        Edit::Names { names } => {
            let mut list = Vec::new();
            for (k, n) in names.into_iter().enumerate() {
                let name = n.name.trim().to_owned();
                if !valid_name(&name) {
                    return Err(bad(
                        &format!("names[{k}].name"),
                        format!("\"{name}\" can't be a name: names start with a letter, hold letters, digits, _ and ., and can't look like a cell (B7)."),
                    ));
                }
                let text = n.reference.trim().trim_start_matches('=').to_owned();
                if text.is_empty() {
                    return Err(bad(&format!("names[{k}].ref"), "Say what the name stands for, like Sales!$B$2:$B$100."));
                }
                if list.iter().any(|d: &DefinedName| d.name.eq_ignore_ascii_case(&name)) {
                    return Err(bad(&format!("names[{k}].name"), format!("The name {name} is given twice.")));
                }
                list.push(DefinedName { name, text, tab: None });
            }
            Box::new(move |wb: &Workbook| {
                for (k, d) in list.iter().enumerate() {
                    if let Err(e) = parse_formula(&d.text, 0, 0, &wb.names()) {
                        return Err(bad(&format!("names[{k}].ref"), format!("{} can't be read: {}", d.text, e.message)));
                    }
                }
                Ok(Some(Op::SetNames { names: list }))
            })
        }
        Edit::Chart { chart } => {
            let kind = chart.kind.trim().to_ascii_lowercase();
            if !CHART_KINDS.contains(&kind.as_str()) {
                return Err(bad("chart.kind", format!("\"{}\" is not a chart kind; use one of {}.", chart.kind, CHART_KINDS.join(", "))));
            }
            let (data_tab, data) = calc_engine::addr::split_tab(&chart.range);
            if Rect::parse(data, MAX_ROWS, MAX_COLS).is_none() {
                return Err(bad("chart.range", format!("\"{}\" is not a range; write it like A1:C20 or Sales!A1:C20.", chart.range)));
            }
            let anchor = match &chart.anchor {
                Some(a) => view::parse_cell(a, "chart.anchor")?,
                None => calc_engine::CellPos::new(0, 0),
            };
            let (width, height) = (chart.width.unwrap_or(480), chart.height.unwrap_or(300));
            if !(20..=4000).contains(&width) || !(20..=4000).contains(&height) {
                return Err(bad("chart.width", "A chart is between 20 and 4,000 pixels wide and high."));
            }
            let title = chart.title.trim().chars().take(300).collect::<String>();
            Box::new(move |wb: &Workbook| {
                let t = tab_of(wb, &chart.tab)?;
                if let Some(name) = &data_tab
                    && wb.tab_by_name(name).is_none()
                {
                    return Err(no_tab_named(name));
                }
                let id = match chart.id {
                    Some(id) => {
                        if !wb.charts.iter().any(|c| c.id == id) {
                            return Err(not_found("not_found", format!("There is no chart with id {id} in this sheet."), "Leave the id out to add a new chart."));
                        }
                        id
                    }
                    None => new_id(),
                };
                Ok(Some(Op::PutChart {
                    chart: Chart {
                        id,
                        tab: t.id,
                        kind,
                        title,
                        range: chart.range.trim().to_owned(),
                        anchor_row: anchor.row,
                        anchor_col: anchor.col,
                        width,
                        height,
                    },
                }))
            })
        }
        Edit::DeleteChart { id } => Box::new(move |wb: &Workbook| {
            if !wb.charts.iter().any(|c| c.id == id) {
                return Err(not_found("not_found", format!("There is no chart with id {id} in this sheet."), "List the tab's charts with GET …/cells."));
            }
            Ok(Some(Op::DeleteChart { id }))
        }),
        Edit::Protect { protection } => {
            let window = match &protection.range {
                Some(r) => Some(view::parse_range(r, "protection.range")?),
                None => None,
            };
            let mut editors = Vec::new();
            for (k, e) in protection.editors.iter().enumerate() {
                let field = format!("protection.editors[{k}]");
                let uuid = match uuid::Uuid::parse_str(e.trim()) {
                    Ok(u) => u,
                    Err(_) => match state.accounts.by_public_id(e).await {
                        Ok(Some(a)) => a.uuid,
                        Ok(None) => return Err(bad(&field, format!("No Carbon or Silicon has the id {e}."))),
                        Err(err) if err.code == "invalid_field" => return Err(bad(&field, err.message)),
                        Err(err) => return Err(err),
                    },
                };
                if !editors.contains(&uuid.to_string()) {
                    editors.push(uuid.to_string());
                }
            }
            let description = protection.description.trim().chars().take(500).collect::<String>();
            let ProtectionIn { id, tab, .. } = protection;
            Box::new(move |wb: &Workbook| {
                let t = tab_of(wb, &tab)?;
                let (area, ext) = match window {
                    Some(w) => {
                        let (a, e) = id_area(t, bounded(t, w));
                        (Some(a), e)
                    }
                    None => (None, None),
                };
                let id = match id {
                    Some(id) => {
                        if !wb.protections.iter().any(|p| p.id == id) {
                            return Err(not_found("not_found", format!("There is no protection with id {id} in this sheet."), "Leave the id out to add a new protection."));
                        }
                        id
                    }
                    None => new_id(),
                };
                Ok(Some(with_extend(ext, Op::PutProtection { protection: Protection { id, tab: t.id, area, editors, description } })))
            })
        }
        Edit::Unprotect { id } => Box::new(move |wb: &Workbook| {
            if !wb.protections.iter().any(|p| p.id == id) {
                return Err(not_found("not_found", format!("There is no protection with id {id} in this sheet."), "List the tab's protections with GET …/cells."));
            }
            Ok(Some(Op::DeleteProtection { id }))
        }),
    }))
}

fn no_tab_named(name: &str) -> ApiError {
    view::no_such_tab(&TabRef::Name(name.to_owned()))
}

/// A width or height: pixels, or none for the default.
fn size(v: Option<f64>, field: &str, max: f64) -> ApiResult<Option<f32>> {
    match v {
        None => Ok(None),
        Some(x) if x.is_finite() && x > 0.0 && x <= max => Ok(Some(x as f32)),
        Some(_) => Err(bad(field, format!("{field} is a number of pixels between 1 and {max}, or null for the default."))),
    }
}

/// Ids for positions `from..from+n` of an axis as they will be once extended.
fn virtual_ids(axis: &Axis, from: u32, to: u32) -> Vec<u32> {
    (from..to).map(|p| axis_id(axis, p)).collect()
}

/// Moves `n` rows (or columns) from `from` to before position `to` (positions before the move), giving positions
/// past the end ids first.
fn move_op(t: &Tab, rows: bool, from: u32, n: u32, to: u32) -> ApiResult<Op> {
    let limit = if rows { MAX_ROWS } else { MAX_COLS };
    if n == 0 || from.saturating_add(n) > limit || to > limit {
        return Err(bad("from", "Those rows or columns are outside the grid."));
    }
    let axis = if rows { &t.rows } else { &t.cols };
    let need = (from + n).max(to);
    let ext = if rows { extend_for(t, need, 0) } else { extend_for(t, 0, need) };
    let (lo, hi) = (from.min(to), (from + n).max(to));
    let moving = virtual_ids(axis, from, from + n);
    let mut seq: Vec<u32> = virtual_ids(axis, lo, hi).into_iter().filter(|id| !moving.contains(id)).collect();
    let insert_at = if to <= from { (to - lo) as usize } else { (to - lo) as usize - n as usize };
    for (i, id) in moving.into_iter().enumerate() {
        seq.insert(insert_at + i, id);
    }
    Ok(with_extend(ext, Op::Reorder { tab: t.id, rows, ids: seq }))
}

/// Positions inside the grid.
fn check_grid<T>(writes: &[(u32, u32, T)]) -> ApiResult<()> {
    if let Some((r, c, _)) = writes.iter().find(|(r, c, _)| *r >= MAX_ROWS || *c >= MAX_COLS) {
        return Err(bad(
            "at",
            format!("Row {} column {} is outside the grid (at most {MAX_ROWS} rows and {MAX_COLS} columns).", r + 1, c + 1),
        ));
    }
    Ok(())
}

/// What happens to rows or columns.
#[derive(Clone, Copy)]
enum AxisEdit {
    Insert,
    Delete,
    /// To before this position (positions before the move).
    Move(u32),
}

/// Inserting, deleting and moving `count` rows (or columns) from position `at`.
fn axis_stage(tab: TabRef, rows: bool, what: AxisEdit, at: u32, count: u32) -> ApiResult<Stage> {
    let (limit, word) = if rows { (MAX_ROWS, "rows") } else { (MAX_COLS, "columns") };
    if count == 0 {
        return Err(bad("count", format!("count is how many {word}: 1 or more.")));
    }
    if at >= limit || at.saturating_add(count) > limit {
        return Err(bad("at", format!("Those {word} are outside the grid (a tab has at most {limit} {word}; positions start at 0).")));
    }
    if let AxisEdit::Move(to) = what
        && to > limit
    {
        return Err(bad("to", format!("Position {to} is outside the grid (a tab has at most {limit} {word}).")));
    }
    if count > 1_000_000 {
        return Err(bad("count", format!("Insert, delete or move at most 1,000,000 {word} at a time.")));
    }
    Ok(match what {
        AxisEdit::Insert => Box::new(move |wb: &Workbook| {
            let t = tab_of(wb, &tab)?;
            Ok(Some(wb.prepare_insert(t.id, rows, at, count)?))
        }),
        AxisEdit::Delete => Box::new(move |wb: &Workbook| {
            let t = tab_of(wb, &tab)?;
            let axis = if rows { &t.rows } else { &t.cols };
            if at >= axis.len() {
                // Only never-written grid is there: nothing to delete.
                return Ok(None);
            }
            Ok(Some(wb.prepare_delete(t.id, rows, at, count)?))
        }),
        AxisEdit::Move(to) => Box::new(move |wb: &Workbook| {
            let t = tab_of(wb, &tab)?;
            if to >= at && to <= at + count {
                // Moving a block to where it already is.
                return Ok(None);
            }
            Ok(Some(move_op(t, rows, at, count, to)?))
        }),
    })
}

/// Hiding or showing rows (`3:5`) or columns (`C:E`).
fn hide_stage(tab: TabRef, rows: bool, hidden: bool, range: &str) -> ApiResult<Stage> {
    let (a, b) = if rows { view::parse_rows(range, "range")? } else { view::parse_cols(range, "range")? };
    if (b - a) as usize >= MAX_CELLS_PER_EDIT {
        return Err(too_many_cells((b - a + 1) as u64));
    }
    Ok(Box::new(move |wb: &Workbook| {
        let t = tab_of(wb, &tab)?;
        let (axis, now) = if rows { (&t.rows, &t.hidden_rows) } else { (&t.cols, &t.hidden_cols) };
        let now: std::collections::HashSet<u32> = now.iter().copied().collect();
        if hidden {
            let ids: Vec<u32> = (a..=b).map(|p| axis_id(axis, p)).filter(|id| !now.contains(id)).collect();
            if ids.is_empty() {
                return Ok(None);
            }
            let ext = if rows { extend_for(t, b + 1, 0) } else { extend_for(t, 0, b + 1) };
            Ok(Some(with_extend(ext, Op::Hide { tab: t.id, rows, ids, hidden })))
        } else {
            // Positions that never had ids can't be hidden.
            let ids: Vec<u32> = (a..=b.min(axis.len().saturating_sub(1)))
                .filter_map(|p| axis.id_at(p))
                .filter(|id| now.contains(id))
                .collect();
            if ids.is_empty() {
                return Ok(None);
            }
            Ok(Some(Op::Hide { tab: t.id, rows, ids, hidden }))
        }
    }))
}
