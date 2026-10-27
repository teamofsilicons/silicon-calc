//! The line routes for cells: `GET {line}/tabs`, `GET {line}/tabs/{tab}/cells` (JSON, CSV or plain values, now or
//! at a version), `GET {line}/tabs/{tab}/cells/{a1}` (one cell with its history), `POST {line}/edits` (every edit
//! type, atomically, and undo/redo of the caller's own last change). See docs/API.md, Cells and Editing.
//!
//! Reads go straight to the line's state in memory (or a past state): a window of a huge tab reads only the cells
//! in the window.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use calc_engine::{CellPos, Rect, Tab, Workbook};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    LineHandle, OpKind,
    ctx::{CellPath, LineCtx, LinePath, TabPath, both},
    edits::{self, Prepared},
    store,
    task::{Job, blocking},
    versions, view,
};
use crate::{
    auth::{Caller, MaybeCaller},
    error::{ApiError, ApiResult, QueryParams, json_problem},
    perms::Role,
    state::AppState,
};

pub(crate) fn router() -> Router<AppState> {
    let r = Router::new();
    let r = both(r, "/tabs", get(tabs));
    let r = both(r, "/tabs/{tab}/cells", get(cells));
    let r = both(r, "/tabs/{tab}/cells/{a1}", get(cell));
    both(r, "/edits", post(edit))
}

/// Where a read comes from: the line as it is now, or the line at a past version.
pub(crate) enum Source {
    Live(LineHandle),
    At(Arc<Workbook>, i64),
}

impl Source {
    /// The line now (`version` none, or the line's current version), else the line at `version`.
    pub(crate) async fn of(state: &AppState, ctx: &LineCtx, version: Option<i64>) -> ApiResult<Self> {
        let handle = ctx.handle(state).await?;
        match version {
            None => Ok(Self::Live(handle)),
            Some(v) if v == handle.version() => Ok(Self::Live(handle)),
            Some(v) => Ok(Self::At(versions::state_at(state, &ctx.line.id, v).await?, v)),
        }
    }

    /// Runs `f` on the workbook, its version and a way to find a tab's used area (kept up to date for the line now,
    /// worked out for a past state), on a blocking thread.
    pub(crate) async fn read<R: Send + 'static>(
        &self,
        f: impl FnOnce(&Workbook, i64, &dyn Fn(&Tab) -> Option<Rect>) -> R + Send + 'static,
    ) -> ApiResult<R> {
        match self {
            Self::Live(h) => h.shared.read(move |st| f(&st.wb, st.version, &|t: &Tab| st.used(t))).await,
            Self::At(wb, v) => {
                let (wb, v) = (wb.clone(), *v);
                blocking(move || f(&wb, v, &|t: &Tab| t.used_rect())).await
            }
        }
    }
}

/// A version as a query gives it: a number, or `head` for the latest.
pub(crate) fn parse_version(text: Option<&str>, field: &str) -> ApiResult<Option<i64>> {
    match text.map(str::trim).filter(|t| !t.is_empty()) {
        None => Ok(None),
        Some(t) if t.eq_ignore_ascii_case("head") || t.eq_ignore_ascii_case("latest") => Ok(None),
        Some(t) => match t.parse::<i64>() {
            Ok(v) if v >= 0 => Ok(Some(v)),
            _ => Err(ApiError::invalid_field(field, format!("\"{t}\" is not a version; versions are numbers (0, 1, 2, …) or head."))),
        },
    }
}

// ------------------------------------------------------------------------------------------------
// Tabs
// ------------------------------------------------------------------------------------------------

/// `{"version", "tabs": [Tab], "names": [{"name", "ref"}]}` for a source.
pub(crate) async fn tabs_answer(state: &AppState, source: &Source) -> ApiResult<Value> {
    let mut v = source
        .read(|wb, version, used| {
            let tabs: Vec<Value> = wb.tabs.iter().enumerate().map(|(i, t)| view::tab_json(t, i, used(t))).collect();
            let names: Vec<Value> = wb.names.iter().map(|n| json!({"name": n.name, "ref": n.text})).collect();
            json!({"version": version, "tabs": tabs, "names": names})
        })
        .await?;
    view::resolve_tab_accounts(state, &mut v["tabs"]).await;
    Ok(v)
}

async fn tabs(State(state): State<AppState>, MaybeCaller(caller): MaybeCaller, Path(p): Path<LinePath>) -> ApiResult<Json<Value>> {
    let ctx = LineCtx::of(&state, caller.as_ref(), &p).await?;
    let source = Source::of(&state, &ctx, None).await?;
    Ok(Json(tabs_answer(&state, &source).await?))
}

// ------------------------------------------------------------------------------------------------
// Cells
// ------------------------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub(crate) struct CellsQuery {
    pub range: Option<String>,
    pub version: Option<String>,
    pub format: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Json,
    Csv,
    Values,
}

fn format_of(q: &CellsQuery, headers: &HeaderMap) -> ApiResult<Format> {
    match q.format.as_deref().map(|f| f.trim().to_ascii_lowercase()) {
        Some(f) if f.is_empty() || f == "json" => Ok(Format::Json),
        Some(f) if f == "csv" => Ok(Format::Csv),
        Some(f) if f == "values" => Ok(Format::Values),
        Some(f) => Err(ApiError::invalid_field("format", format!("There is no format \"{f}\" for cells; use json, csv or values."))),
        None => {
            let accept = headers.get(header::ACCEPT).and_then(|h| h.to_str().ok()).unwrap_or("");
            Ok(if accept.contains("text/csv") { Format::Csv } else { Format::Json })
        }
    }
}

/// The cells answer (JSON, CSV or values) for a tab of a source.
pub(crate) async fn cells_answer(
    state: &AppState,
    source: &Source,
    tab: view::TabRef,
    q: &CellsQuery,
    headers: &HeaderMap,
) -> ApiResult<Response> {
    let format = format_of(q, headers)?;
    let window = match q.range.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) => Some(view::parse_range(r, "range")?),
        None => None,
    };
    match format {
        Format::Json => {
            let body = source
                .read(move |wb, version, used| -> ApiResult<view::CellsBody> {
                    let t = view::find_tab(wb, &tab)?;
                    let index = wb.tabs.iter().position(|x| x.id == t.id).unwrap_or(0);
                    let u = used(t);
                    view::cells_body(wb, version, t, index, u, view::effective(window, u))
                })
                .await??;
            Ok(Json(view::finish_cells(state, body).await).into_response())
        }
        Format::Values => {
            let body = source
                .read(move |wb, version, used| -> ApiResult<Value> {
                    let t = view::find_tab(wb, &tab)?;
                    view::values_body(t, version, view::effective(window, used(t)))
                })
                .await??;
            Ok(Json(body).into_response())
        }
        Format::Csv => {
            let (text, version) = source
                .read(move |wb, version, used| -> ApiResult<(String, i64)> {
                    let t = view::find_tab(wb, &tab)?;
                    Ok((view::csv_body(wb, t, view::effective(window, used(t)))?, version))
                })
                .await??;
            let mut res = (StatusCode::OK, text).into_response();
            res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/csv; charset=utf-8"));
            if let Ok(v) = HeaderValue::from_str(&version.to_string()) {
                res.headers_mut().insert("x-calc-version", v);
            }
            Ok(res)
        }
    }
}

async fn cells(
    State(state): State<AppState>,
    MaybeCaller(caller): MaybeCaller,
    Path(p): Path<TabPath>,
    headers: HeaderMap,
    QueryParams(q): QueryParams<CellsQuery>,
) -> ApiResult<Response> {
    let ctx = LineCtx::load(&state, caller.as_ref(), &p.sheet, p.copy.as_deref()).await?;
    let version = parse_version(q.version.as_deref(), "version")?;
    let source = Source::of(&state, &ctx, version).await?;
    cells_answer(&state, &source, view::TabRef::parse(&p.tab), &q, &headers).await
}

// ------------------------------------------------------------------------------------------------
// One cell and its history
// ------------------------------------------------------------------------------------------------

/// How many changes of one cell are shown.
const CELL_HISTORY: i64 = 20;

#[derive(Deserialize)]
struct CellQuery {
    version: Option<String>,
}

/// What one cell is, and where: answered from the line's state.
struct OneCell {
    json: Value,
    tab: Value,
    uuids: Vec<String>,
    tab_id: u32,
    ids: Option<(u32, u32)>,
    /// Its position, for checking which bulk changes covered it.
    pos: (u32, u32),
}

async fn cell(
    State(state): State<AppState>,
    MaybeCaller(caller): MaybeCaller,
    Path(p): Path<CellPath>,
    QueryParams(q): QueryParams<CellQuery>,
) -> ApiResult<Json<Value>> {
    let ctx = LineCtx::load(&state, caller.as_ref(), &p.sheet, p.copy.as_deref()).await?;
    let at = view::parse_cell(&p.a1, "a1")?;
    let version = parse_version(q.version.as_deref(), "version")?;
    let source = Source::of(&state, &ctx, version).await?;
    let tab_ref = view::TabRef::parse(&p.tab);
    let one = source
        .read(move |wb, version, used| -> ApiResult<OneCell> {
            let t = view::find_tab(wb, &tab_ref)?;
            let index = wb.tabs.iter().position(|x| x.id == t.id).unwrap_or(0);
            let cell = t.cell_at(at);
            let mut styles = std::collections::BTreeSet::new();
            let mut uuids = std::collections::BTreeSet::new();
            let mut json = view::cell_json(wb, t, at.row, at.col, &cell, &mut styles, &mut uuids);
            json["version"] = json!(version);
            json["style"] = view::style_json(wb, cell.style);
            json["styles"] = view::styles_json(wb, &styles);
            let ids = t.rows.id_at(at.row).zip(t.cols.id_at(at.col));
            Ok(OneCell {
                json,
                tab: view::tab_json(t, index, used(t)),
                uuids: uuids.into_iter().collect(),
                tab_id: t.id,
                ids,
                pos: (at.row, at.col),
            })
        })
        .await??;
    let OneCell { mut json, mut tab, uuids, tab_id, ids, pos } = one;
    let up_to = json["version"].as_i64().unwrap_or(0);
    let mut history: Vec<Value> = Vec::new();
    let mut accounts_wanted: Vec<Uuid> = uuids.iter().filter_map(|u| Uuid::parse_str(u).ok()).collect();
    if let Some((row, col)) = ids {
        let changes = store::cell_changes(&state, &ctx.line.id, tab_id, row, col, up_to, CELL_HISTORY).await?;
        // Bulk changes count when the cell was inside the area they covered.
        let areas: Vec<Option<(u32, u32, u32, u32)>> = changes.iter().filter_map(|c| c.bulk).collect();
        let inside: Vec<bool> = source
            .read(move |wb, _, _| {
                let Some(t) = wb.tab(tab_id) else { return vec![false; areas.len()] };
                areas
                    .iter()
                    .map(|a| match a {
                        None => true,
                        Some((r1, c1, r2, c2)) => match (t.rows.pos_of(*r1), t.cols.pos_of(*c1), t.rows.pos_of(*r2), t.cols.pos_of(*c2)) {
                            (Some(a), Some(b), Some(c), Some(d)) => Rect::new(a, b, c, d).contains(CellPos::new(pos.0, pos.1)),
                            _ => false,
                        },
                    })
                    .collect()
            })
            .await?;
        let mut inside = inside.into_iter();
        for c in changes {
            if c.bulk.is_some() && !inside.next().unwrap_or(false) {
                continue;
            }
            accounts_wanted.push(c.by);
            let mut item = json!({
                "version": c.version, "by": c.by.to_string(), "at": crate::db::rfc3339(c.at),
                "before": c.before.unwrap_or(Value::Null), "after": c.after.unwrap_or(Value::Null),
                "summary": c.summary,
            });
            if c.bulk.is_some() {
                item["bulk"] = json!(true);
            }
            history.push(item);
            if history.len() as i64 >= CELL_HISTORY {
                break;
            }
        }
    }
    let accounts = state.accounts.get_map(accounts_wanted).await;
    for h in history.iter_mut() {
        if let Some(u) = h["by"].as_str().and_then(|u| Uuid::parse_str(u).ok()) {
            h["by"] = view::account_json(accounts.get(&u), u);
        }
    }
    let map: serde_json::Map<String, Value> =
        uuids.iter().filter_map(|u| Uuid::parse_str(u).ok().map(|id| (u.clone(), view::account_json(accounts.get(&id), id)))).collect();
    view::resolve_tab_accounts(&state, &mut tab).await;
    json["tab"] = tab;
    json["accounts"] = Value::Object(map);
    json["history"] = Value::Array(history);
    Ok(Json(json))
}

// ------------------------------------------------------------------------------------------------
// Editing
// ------------------------------------------------------------------------------------------------

async fn edit(State(state): State<AppState>, caller: Caller, Path(p): Path<LinePath>, body: axum::body::Bytes) -> ApiResult<Json<Value>> {
    let ctx = LineCtx::of(&state, Some(&caller), &p).await?;
    ctx.require_edit()?;
    let text: &[u8] = if body.iter().all(u8::is_ascii_whitespace) { b"{}" } else { &body };
    let raw: Value = serde_json::from_slice(text).map_err(|e| json_problem(&e))?;
    let parsed = edits::parse(&state, raw).await?;
    if let Some(i) = parsed.owner_only
        && ctx.access.role != Role::Owner
    {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "not_allowed",
            format!("Edit {i}: only the owner of \"{}\" can protect or unprotect ranges.", ctx.access.sheet.title),
            "Ask the sheet's owner to set up the protection.",
        )
        .with_details(json!({"index": i})));
    }
    let handle = ctx.handle(&state).await?;
    let edits::Parsed { base_version, client_id, items, .. } = parsed;
    let outcome = match items.as_slice() {
        [Prepared::Undo] => handle.shared.undo(caller.uuid, false, base_version, client_id).await?,
        [Prepared::Redo] => handle.shared.undo(caller.uuid, true, base_version, client_id).await?,
        _ => {
            let n = items.len();
            // `edits::parse` keeps undo and redo alone in a request, so everything here is a stage.
            let stages = items
                .into_iter()
                .filter_map(|i| match i {
                    Prepared::Stage(s) => Some(s),
                    Prepared::Undo | Prepared::Redo => None,
                })
                .collect();
            let mut job = Job::new(caller.uuid, OpKind::Edit, stages);
            job.base_version = base_version;
            job.client_id = client_id;
            job.edits = n;
            handle.shared.run(job).await?
        }
    };
    Ok(Json(json!({
        "version": outcome.version, "changes": outcome.changes, "overwrote": outcome.overwrote, "skipped": outcome.skipped,
    })))
}
