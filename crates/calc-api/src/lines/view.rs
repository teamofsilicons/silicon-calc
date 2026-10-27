//! The JSON answers of lines: tabs, cells (JSON, CSV and plain values), charts, protections and accounts.
//!
//! Reading a window of a huge tab only looks at the cells in the window: positions are turned into ids through
//! the tab's axes and each cell is read from its column chunk, so nothing scans the whole tab (the used area is
//! cached per open line, see `registry::LineState::used`).

use std::collections::{BTreeMap, BTreeSet};

use axum::http::StatusCode;
use calc_engine::{
    Cell, CellPos, Rect, Tab, TabId, Workbook,
    addr::{MAX_COLS, MAX_ROWS, col_index, col_name},
    model::{Chart, Protection},
    numfmt,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

use super::registry::LineState;
use crate::{
    accounts::Account,
    db::rfc3339,
    error::{ApiError, ApiResult},
    state::AppState,
};

/// The grid shown is at least this big (docs/API.md `Tab`).
const MIN_ROWS: u32 = 1000;
const MIN_COLS: u32 = 26;
/// The most cells one JSON (or values) answer covers; bigger reads go through `format=csv` or several windows.
pub(crate) const MAX_CELLS_ANSWER: u64 = 1_000_000;
/// The most cells one CSV answer covers.
pub(crate) const MAX_CELLS_CSV: u64 = 60_000_000;

/// A tab as a request names it: by id (`1`) or by name (`"Sales"`).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub(crate) enum TabRef {
    Id(u32),
    Name(String),
}

impl TabRef {
    /// From a path segment or query value: a number is an id unless a tab has that name.
    pub(crate) fn parse(text: &str) -> Self {
        match text.trim().parse::<u32>() {
            Ok(id) => Self::Id(id),
            Err(_) => Self::Name(text.trim().to_owned()),
        }
    }

    pub(crate) fn describe(&self) -> String {
        match self {
            Self::Id(id) => format!("with id {id}"),
            Self::Name(n) => format!("named \"{n}\""),
        }
    }
}

/// The tab a request names, in `wb`.
pub(crate) fn find_tab<'a>(wb: &'a Workbook, r: &TabRef) -> ApiResult<&'a Tab> {
    let found = match r {
        TabRef::Id(id) => wb.tab(*id).or_else(|| wb.tab_by_name(&id.to_string())),
        TabRef::Name(n) => wb
            .tab_by_name(n)
            .or_else(|| n.parse::<u32>().ok().and_then(|id| wb.tab(id))),
    };
    found.ok_or_else(|| no_such_tab(r))
}

pub(crate) fn no_such_tab(r: &TabRef) -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "no_such_tab",
        format!("There is no tab {} in this sheet.", r.describe()),
        "List the tabs (GET …/tabs, or calc tabs) and use one of their ids or names.",
    )
}

/// A time kept as Unix seconds, as answers show it.
pub(crate) fn unix_rfc3339(secs: i64) -> String {
    OffsetDateTime::from_unix_timestamp(secs)
        .map(rfc3339)
        .unwrap_or_default()
}

/// An account as answers show it, or the "deleted account" stand-in.
pub(crate) fn account_json(a: Option<&Account>, uuid: Uuid) -> Value {
    serde_json::to_value(a.cloned().unwrap_or_else(|| Account::unknown(uuid)))
        .unwrap_or(Value::Null)
}

/// An account known only by its uuid, until `resolve_tab_accounts` fills it in.
fn account_stub(uuid: &str) -> Value {
    json!({"uuid": uuid, "id": "", "kind": "", "display_name": "", "pfp_url": ""})
}

/// docs/API.md `Tab`. `used` is the tab's used area (cached by open lines).
pub(crate) fn tab_json(tab: &Tab, index: usize, used: Option<Rect>) -> Value {
    let code = match &tab.code {
        None => Value::Null,
        Some(c) => json!({
            "language": c.language, "code": c.code, "inputs": c.inputs, "run_by": account_stub(&c.run_by),
            "run_at": unix_rfc3339(c.run_at),
        }),
    };
    json!({
        "id": tab.id, "name": tab.name, "index": index,
        "rows": used.map_or(0, |r| r.r2 + 1).max(MIN_ROWS), "cols": used.map_or(0, |r| r.c2 + 1).max(MIN_COLS),
        "used": used.map(|r| r.to_string()), "frozen_rows": tab.frozen_rows, "frozen_cols": tab.frozen_cols,
        "color": tab.color, "code": code,
        "format_rules":tab.format_rules.iter().filter_map(|r|Some(json!({"id":r.id,"range":tab.pos_rect(&r.area)?.to_string(),"rule":r.kind}))).collect::<Vec<_>>(),
    })
}

/// Every tab of an open line.
pub(crate) fn tabs_json(st: &LineState) -> Vec<Value> {
    st.wb
        .tabs
        .iter()
        .enumerate()
        .map(|(i, t)| tab_json(t, i, st.used(t)))
        .collect()
}

fn run_by_uuids(v: &Value, out: &mut Vec<Uuid>) {
    match v {
        Value::Array(list) => list.iter().for_each(|t| run_by_uuids(t, out)),
        Value::Object(_) => {
            if let Some(u) = v
                .pointer("/code/run_by/uuid")
                .and_then(Value::as_str)
                .and_then(|u| Uuid::parse_str(u).ok())
            {
                out.push(u);
            }
        }
        _ => {}
    }
}

fn fill_run_by(v: &mut Value, accounts: &std::collections::HashMap<Uuid, Account>) {
    match v {
        Value::Array(list) => list.iter_mut().for_each(|t| fill_run_by(t, accounts)),
        Value::Object(_) => {
            if let Some(slot) = v.pointer_mut("/code/run_by")
                && let Some(u) = slot
                    .get("uuid")
                    .and_then(Value::as_str)
                    .and_then(|u| Uuid::parse_str(u).ok())
            {
                *slot = account_json(accounts.get(&u), u);
            }
        }
        _ => {}
    }
}

/// Fills in the `code.run_by` accounts of `Tab` JSON (one tab, or an array of them).
pub(crate) async fn resolve_tab_accounts(state: &AppState, tabs: &mut Value) {
    let mut uuids = Vec::new();
    run_by_uuids(tabs, &mut uuids);
    if uuids.is_empty() {
        return;
    }
    let accounts = state.accounts.get_map(uuids).await;
    fill_run_by(tabs, &accounts);
}

/// A cell's value as answers show it (`{"t": "number", "v": 12.5}`).
pub(crate) fn value_json(v: &calc_engine::Value) -> Value {
    serde_json::to_value(v).unwrap_or_else(|_| json!({"t": "empty"}))
}

/// What a cell shows, formatted with its number format.
pub(crate) fn shown(wb: &Workbook, cell: &Cell) -> String {
    numfmt::format_value(&cell.value, wb.style_of(cell.style).num_fmt.as_deref()).0
}

/// Whether a cell has anything to show: a value, a formula or a look.
pub(crate) fn is_blank(cell: &Cell) -> bool {
    cell.value.is_empty() && cell.formula.is_none() && cell.style == 0
}

/// One cell in the cells-answer shape. Collects the style ids and author uuids it names.
pub(crate) fn cell_json(
    wb: &Workbook,
    tab: &Tab,
    r: u32,
    c: u32,
    cell: &Cell,
    styles: &mut BTreeSet<u32>,
    uuids: &mut BTreeSet<String>,
) -> Value {
    let mut v = Map::new();
    v.insert("a".into(), json!(CellPos::new(r, c).to_string()));
    v.insert("r".into(), json!(r));
    v.insert("c".into(), json!(c));
    v.insert("v".into(), value_json(&cell.value));
    let (text, color) = numfmt::format_value(&cell.value, wb.style_of(cell.style).num_fmt.as_deref());
    v.insert("t".into(), json!(text));
    if let Some(color) = color { v.insert("tc".into(), json!(color)); }
    if cell.formula.is_some()
        && let Some(f) = wb.formula_text(tab.id, r, c)
    {
        v.insert("f".into(), json!(format!("={f}")));
    }
    if cell.style != 0 {
        v.insert("s".into(), json!(cell.style));
        styles.insert(cell.style);
    }
    if wb.has_format_rule(tab,r,c) {v.insert("cs".into(),json!(wb.displayed_style(tab,r,c)));}
    if cell.spilled {
        v.insert("spill".into(), json!(true));
    }
    if let Some((by, at)) = wb.cell_author(cell.meta) {
        v.insert("by".into(), json!(by));
        v.insert("at".into(), json!(unix_rfc3339(at)));
        uuids.insert(by);
    }
    Value::Object(v)
}

/// The engine's `Style` JSON for a style id.
pub(crate) fn style_json(wb: &Workbook, id: u32) -> Value {
    serde_json::to_value(wb.style_of(id)).unwrap_or(Value::Null)
}

/// The styles a set of ids names: `{"3": Style}`.
pub(crate) fn styles_json(wb: &Workbook, ids: &BTreeSet<u32>) -> Value {
    Value::Object(
        ids.iter()
            .map(|s| (s.to_string(), style_json(wb, *s)))
            .collect(),
    )
}

/// A protection: `{"id", "tab", "range" (null = the whole tab), "editors": [uuid], "description"}`.
pub(crate) fn protection_json(wb: &Workbook, p: &Protection) -> Value {
    let range = p
        .area
        .as_ref()
        .and_then(|a| wb.tab(p.tab).and_then(|t| t.pos_rect(a)))
        .map(|r| r.to_string());
    json!({"id": p.id, "tab": p.tab, "range": range, "editors": p.editors, "description": p.description})
}

/// A chart: `{"id", "tab", "kind", "title", "range", "anchor": "E2", "width", "height"}`.
pub(crate) fn chart_json(c: &Chart) -> Value {
    json!({
        "id": c.id, "tab": c.tab, "kind": c.kind, "title": c.title, "range": c.range,
        "anchor": CellPos::new(c.anchor_row, c.anchor_col).to_string(), "anchor_row": c.anchor_row,
        "anchor_col": c.anchor_col, "width": c.width, "height": c.height,
    })
}

/// A range as a request gives it, parsed. Whole columns (`A:C`) and rows (`3:5`) are open on one axis: answers
/// stop them at the tab's used area.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Window {
    pub rect: Rect,
    pub open_rows: bool,
    pub open_cols: bool,
}

/// Parses an A1 range (`B7`, `A1:C9`, `A:C`, `3:5`, `A1:A`).
pub(crate) fn parse_range(text: &str, field: &str) -> ApiResult<Window> {
    let t = text.trim();
    let (_, rest) = calc_engine::addr::split_tab(t);
    let rect = Rect::parse(rest, MAX_ROWS, MAX_COLS).ok_or_else(|| {
        ApiError::invalid_field(
            field,
            format!("\"{text}\" is not a range; ranges look like B7, A1:C9, A:C or 3:5."),
        )
    })?;
    Ok(Window {
        rect,
        open_rows: rect.r2 == MAX_ROWS - 1,
        open_cols: rect.c2 == MAX_COLS - 1,
    })
}

/// Parses one cell address (`B7`).
pub(crate) fn parse_cell(text: &str, field: &str) -> ApiResult<CellPos> {
    let (_, rest) = calc_engine::addr::split_tab(text.trim());
    CellPos::parse(rest).ok_or_else(|| {
        ApiError::invalid_field(
            field,
            format!("\"{text}\" is not a cell address; addresses look like B7."),
        )
    })
}

/// Parses column letters (`C`) or a column range (`A:C`) into positions.
pub(crate) fn parse_cols(text: &str, field: &str) -> ApiResult<(u32, u32)> {
    let t = text.trim().replace('$', "");
    let (a, b) = t.split_once(':').unwrap_or((t.as_str(), t.as_str()));
    match (col_index(a.trim()), col_index(b.trim())) {
        (Some(x), Some(y)) => Ok((x.min(y), x.max(y))),
        _ => match parse_range(text, field) {
            Ok(w) => Ok((w.rect.c1, w.rect.c2)),
            Err(_) => Err(ApiError::invalid_field(
                field,
                format!("\"{text}\" is not a column range; write it like A:C or C."),
            )),
        },
    }
}

/// Parses row numbers (`3`) or a row range (`2:4`) into positions.
pub(crate) fn parse_rows(text: &str, field: &str) -> ApiResult<(u32, u32)> {
    let t = text.trim().replace('$', "");
    let (a, b) = t.split_once(':').unwrap_or((t.as_str(), t.as_str()));
    match (a.trim().parse::<u32>(), b.trim().parse::<u32>()) {
        (Ok(x), Ok(y)) if x >= 1 && y >= 1 && x <= MAX_ROWS && y <= MAX_ROWS => {
            Ok((x.min(y) - 1, x.max(y) - 1))
        }
        _ => match parse_range(text, field) {
            Ok(w) => Ok((w.rect.r1, w.rect.r2)),
            Err(_) => Err(ApiError::invalid_field(
                field,
                format!("\"{text}\" is not a row range; write it like 2:4 or 3."),
            )),
        },
    }
}

/// The area an answer covers: the range asked for (open axes stopped at the used area), or the used area.
/// None when there is nothing to answer (an empty tab and no range).
pub(crate) fn effective(window: Option<Window>, used: Option<Rect>) -> Option<Rect> {
    match window {
        None => used,
        Some(w) => {
            let mut r = w.rect;
            if w.open_rows {
                r.r2 = used.map_or(r.r1, |u| u.r2.max(r.r1));
            }
            if w.open_cols {
                r.c2 = used.map_or(r.c1, |u| u.c2.max(r.c1));
            }
            Some(r)
        }
    }
}

fn area_cells(r: &Rect) -> u64 {
    r.rows() as u64 * r.cols() as u64
}

fn too_big(cells: u64, max: u64) -> ApiError {
    ApiError::too_large(
        format!("This range covers {cells} cells, more than one answer gives ({max})."),
        "Ask for a smaller range (range=A1:Z10000), or download it as CSV (format=csv) or as a file.",
    )
}

/// The cells answer before accounts are filled in.
pub(crate) struct CellsBody {
    pub json: Value,
    /// The uuids in `by` and among protection editors.
    pub uuids: BTreeSet<String>,
}

/// docs/API.md cells answer for `tab` over `area` (none: the tab is empty and no range was asked for).
pub(crate) fn cells_body(
    wb: &Workbook,
    version: i64,
    tab: &Tab,
    index: usize,
    used: Option<Rect>,
    area: Option<Rect>,
) -> ApiResult<CellsBody> {
    if let Some(a) = &area
        && area_cells(a) > MAX_CELLS_ANSWER
    {
        return Err(too_big(area_cells(a), MAX_CELLS_ANSWER));
    }
    let mut styles = BTreeSet::new();
    let mut uuids = BTreeSet::new();
    let mut cells = Vec::new();
    let (rows, cols) = (tab.rows.len(), tab.cols.len());
    if let Some(a) = &area
        && rows > 0
        && cols > 0
        && a.r1 < rows
        && a.c1 < cols
    {
        let (r2, c2) = (a.r2.min(rows - 1), a.c2.min(cols - 1));
        for r in a.r1..=r2 {
            for c in a.c1..=c2 {
                let cell = tab.cell_at(CellPos::new(r, c));
                if is_blank(&cell) && !wb.has_format_rule(tab,r,c) {
                    continue;
                }
                cells.push(cell_json(wb, tab, r, c, &cell, &mut styles, &mut uuids));
            }
        }
    }
    let in_rows = |p: u32| area.is_some_and(|a| p >= a.r1 && p <= a.r2);
    let col_widths: Map<String, Value> = tab
        .col_widths
        .iter()
        .filter_map(|(&id, &w)| Some((col_name(tab.cols.pos_of(id)?), json!(w))))
        .collect();
    let row_heights: Map<String, Value> = tab
        .row_heights
        .iter()
        .filter_map(|(&id, &h)| {
            let p = tab.rows.pos_of(id)?;
            in_rows(p).then(|| ((p + 1).to_string(), json!(h)))
        })
        .collect();
    let mut hidden_rows: Vec<u32> = tab
        .hidden_rows
        .iter()
        .filter_map(|&id| tab.rows.pos_of(id))
        .filter(|&p| in_rows(p))
        .map(|p| p + 1)
        .collect();
    hidden_rows.sort_unstable();
    let mut hidden_cols: Vec<u32> = tab
        .hidden_cols
        .iter()
        .filter_map(|&id| tab.cols.pos_of(id))
        .collect();
    hidden_cols.sort_unstable();
    let merges: Vec<String> = tab
        .merges
        .iter()
        .filter_map(|m| tab.pos_rect(m))
        .filter(|m| area.is_some_and(|a| a.intersects(m)))
        .map(|m| m.to_string())
        .collect();
    let charts: Vec<Value> = wb
        .charts
        .iter()
        .filter(|c| c.tab == tab.id)
        .map(chart_json)
        .collect();
    let protections: Vec<Value> = wb
        .protections
        .iter()
        .filter(|p| p.tab == tab.id)
        .map(|p| {
            uuids.extend(p.editors.iter().cloned());
            protection_json(wb, p)
        })
        .collect();
    let json = json!({
        "version": version,
        "tab": tab_json(tab, index, used),
        "range": area.map(|a| a.to_string()),
        "cells": cells,
        "styles": styles_json(wb, &styles),
        "accounts": {},
        "col_widths": col_widths,
        "row_heights": row_heights,
        "hidden_rows": hidden_rows,
        "hidden_cols": hidden_cols.into_iter().map(col_name).collect::<Vec<_>>(),
        "merges": merges,
        "frozen_rows": tab.frozen_rows,
        "frozen_cols": tab.frozen_cols,
        "charts": charts,
        "protections": protections,
    });
    Ok(CellsBody { json, uuids })
}

/// Fills in the accounts of a cells answer (and the editors of its protections, as accounts).
pub(crate) async fn finish_cells(state: &AppState, body: CellsBody) -> Value {
    let CellsBody { mut json, uuids } = body;
    let parsed: Vec<Uuid> = uuids
        .iter()
        .filter_map(|u| Uuid::parse_str(u).ok())
        .collect();
    let accounts = state.accounts.get_map(parsed).await;
    let by_cells: BTreeSet<String> = json["cells"]
        .as_array()
        .map(|cells| {
            cells
                .iter()
                .filter_map(|c| c["by"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let map: Map<String, Value> = by_cells
        .iter()
        .filter_map(|u| {
            Uuid::parse_str(u)
                .ok()
                .map(|id| (u.clone(), account_json(accounts.get(&id), id)))
        })
        .collect();
    json["accounts"] = Value::Object(map);
    if let Some(list) = json["protections"].as_array_mut() {
        for p in list {
            if let Some(eds) = p["editors"].as_array_mut() {
                for e in eds.iter_mut() {
                    if let Some(id) = e.as_str().and_then(|u| Uuid::parse_str(u).ok()) {
                        *e = account_json(accounts.get(&id), id);
                    }
                }
            }
        }
    }
    let mut tab = json["tab"].take();
    resolve_tab_accounts(state, &mut tab).await;
    json["tab"] = tab;
    json
}

/// A value for `format=values`: numbers, strings, bools and nulls (errors as their text, formulas as results).
fn plain(v: &calc_engine::Value) -> Value {
    match v {
        calc_engine::Value::Empty => Value::Null,
        calc_engine::Value::Number(n) => serde_json::Number::from_f64(*n)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        calc_engine::Value::Text(s) => Value::String(s.to_string()),
        calc_engine::Value::Bool(b) => Value::Bool(*b),
        calc_engine::Value::Error(e) => Value::String(e.as_str().to_owned()),
    }
}

/// `format=values`: the range as rows of plain values.
pub(crate) fn values_body(tab: &Tab, version: i64, area: Option<Rect>) -> ApiResult<Value> {
    let Some(a) = area else {
        return Ok(json!({"version": version, "range": null, "values": []}));
    };
    if area_cells(&a) > MAX_CELLS_ANSWER {
        return Err(too_big(area_cells(&a), MAX_CELLS_ANSWER));
    }
    let mut rows = Vec::with_capacity(a.rows() as usize);
    for r in a.r1..=a.r2 {
        let row: Vec<Value> = (a.c1..=a.c2).map(|c| plain(&tab.value_at(r, c))).collect();
        rows.push(Value::Array(row));
    }
    Ok(json!({"version": version, "range": a.to_string(), "values": rows}))
}

fn csv_field(out: &mut String, text: &str) {
    if text.contains([',', '"', '\n', '\r']) || text.starts_with(' ') || text.ends_with(' ') {
        out.push('"');
        out.push_str(&text.replace('"', "\"\""));
        out.push('"');
    } else {
        out.push_str(text);
    }
}

/// `format=csv`: the range's shown values as CSV.
pub(crate) fn csv_body(wb: &Workbook, tab: &Tab, area: Option<Rect>) -> ApiResult<String> {
    let Some(a) = area else {
        return Ok(String::new());
    };
    if area_cells(&a) > MAX_CELLS_CSV {
        return Err(too_big(area_cells(&a), MAX_CELLS_CSV));
    }
    let mut out = String::new();
    for r in a.r1..=a.r2 {
        for c in a.c1..=a.c2 {
            if c > a.c1 {
                out.push(',');
            }
            let cell = tab.cell_at(CellPos::new(r, c));
            if !cell.value.is_empty() {
                csv_field(&mut out, &shown(wb, &cell));
            }
        }
        out.push_str("\r\n");
    }
    Ok(out)
}

/// What viewers are told about cells: `{"tab": 1, …cell}` for each position, with styles and author uuids.
pub(crate) fn event_cells(
    wb: &Workbook,
    positions: &BTreeSet<(TabId, u32, u32)>,
) -> (Vec<Value>, Value, BTreeSet<String>) {
    let mut styles: BTreeSet<u32> = BTreeSet::new();
    let mut uuids: BTreeSet<String> = BTreeSet::new();
    let mut cells = Vec::with_capacity(positions.len());
    for &(t, r, c) in positions {
        let Some(tab) = wb.tab(t) else { continue };
        let cell = tab.cell_at(CellPos::new(r, c));
        let mut v = cell_json(wb, tab, r, c, &cell, &mut styles, &mut uuids);
        v["tab"] = json!(t);
        cells.push(v);
    }
    (cells, styles_json(wb, &styles), uuids)
}

/// `{uuid: Account}` for a set of uuids.
pub(crate) async fn accounts_map(
    state: &AppState,
    uuids: impl IntoIterator<Item = Uuid>,
) -> (Value, std::collections::HashMap<Uuid, Account>) {
    let accounts = state.accounts.get_map(uuids).await;
    let map: BTreeMap<String, Value> = accounts
        .iter()
        .map(|(u, a)| (u.to_string(), account_json(Some(a), *u)))
        .collect();
    (
        serde_json::to_value(map).unwrap_or_else(|_| json!({})),
        accounts,
    )
}
