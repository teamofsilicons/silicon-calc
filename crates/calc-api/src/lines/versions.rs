//! Versions: the state of a line at any version (cached), and the version routes.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::{get, post},
};
use calc_engine::Workbook;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    ctx::{LineCtx, LinePath, VersionPath, VersionTabPath, both},
    registry,
    routes::{self, CellsQuery, Source},
    store,
    task::blocking,
    view,
};
use crate::{
    auth::{Caller, MaybeCaller},
    error::{ApiError, ApiResult, JsonBody, QueryParams},
    state::AppState,
};

pub(crate) fn router() -> Router<AppState> {
    let r = both(Router::new(), "/versions", get(list));
    let r = both(r, "/versions/{v}/name", post(name).delete(unname));
    let r = both(r, "/versions/{v}/tabs", get(tabs));
    let r = both(r, "/versions/{v}/tabs/{tab}/cells", get(cells));
    both(r, "/compare", get(compare))
}

/// 404 for a version a line doesn't have (yet).
pub(crate) fn no_such_version(v: i64, head: i64) -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "no_such_version",
        format!("There is no version {v}: this sheet is at version {head}."),
        "List the versions (GET …/versions, or calc versions) and use one of their numbers.",
    )
}

/// A line's latest saved version.
pub(crate) async fn head_of(state: &AppState, line_id: &str) -> ApiResult<i64> {
    let row: Option<(i64,)> = sqlx::query_as("select head from lines where id = $1")
        .bind(line_id)
        .fetch_optional(&state.db)
        .await?;
    row.map(|r| r.0)
        .ok_or_else(|| ApiError::not_found("This sheet"))
}

/// The workbook of a line at `version`, shared: a past state kept in memory, the open line itself when it is at that
/// version, or the nearest snapshot (or kept state) before it with the operations after it replayed.
pub(crate) async fn state_at(
    state: &AppState,
    line_id: &str,
    version: i64,
) -> ApiResult<Arc<Workbook>> {
    let head = head_of(state, line_id).await?;
    if version < 0 || version > head {
        return Err(no_such_version(version, head));
    }
    if let Some(wb) = state
        .lines
        .inner
        .states
        .lock()
        .expect("states lock")
        .get(line_id, version)
    {
        return Ok(wb);
    }
    if let Some(h) = registry::get_open(&state.lines, line_id) {
        let now = h
            .shared
            .read(move |st| (st.version == version).then(|| st.wb.clone()))
            .await?;
        if let Some(wb) = now {
            let wb = Arc::new(wb);
            state
                .lines
                .inner
                .states
                .lock()
                .expect("states lock")
                .put(line_id, version, wb.clone());
            return Ok(wb);
        }
    }
    let floor = store::snapshot_version_at_most(state, line_id, version)
        .await?
        .unwrap_or(0);
    let kept = state
        .lines
        .inner
        .states
        .lock()
        .expect("states lock")
        .nearest(line_id, version, floor);
    let (from, wb) = match kept {
        Some((v, wb)) => (v, blocking(move || (*wb).clone()).await?),
        None => store::snapshot_at_most(state, line_id, Some(version)).await?,
    };
    let (wb, _) = store::replay_ops(state, line_id, wb, from, Some(version)).await?;
    let wb = Arc::new(wb);
    state
        .lines
        .inner
        .states
        .lock()
        .expect("states lock")
        .put(line_id, version, wb.clone());
    Ok(wb)
}

pub(crate) fn version_number(text: &str) -> ApiResult<i64> {
    routes::parse_version(Some(text), "version")?
        .ok_or_else(|| ApiError::invalid_field("version", "Give a version number."))
}

#[derive(Deserialize, Default)]
struct ListQuery {
    cursor: Option<i64>,
    limit: Option<usize>,
}

async fn list(
    State(state): State<AppState>,
    MaybeCaller(caller): MaybeCaller,
    Path(p): Path<LinePath>,
    QueryParams(q): QueryParams<ListQuery>,
) -> ApiResult<Json<Value>> {
    let ctx = LineCtx::of(&state, caller.as_ref(), &p).await?;
    let limit = q.limit.unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(ApiError::invalid_field(
            "limit",
            "Choose a limit from 1 to 200.",
        ));
    }
    if q.cursor.is_some_and(|c| c < 0) {
        return Err(ApiError::invalid_field(
            "cursor",
            "Use the next_cursor from an earlier answer.",
        ));
    }
    let names: Vec<(i64, String)> =
        sqlx::query_as("select seq, name from version_names where line_id = $1")
            .bind(&ctx.line.id)
            .fetch_all(&state.db)
            .await?;
    let names: std::collections::HashMap<_, _> = names.into_iter().collect();
    let mut before = q.cursor.unwrap_or(ctx.line.head + 1);
    let mut groups: Vec<Vec<store::OpListing>> = Vec::new();
    'pages: loop {
        let rows = store::ops_before(&state, &ctx.line.id, before, 500).await?;
        let done = rows.len() < 500;
        for row in rows {
            before = row.seq;
            let join = groups.last().and_then(|g| g.last()).is_some_and(|last| {
                row.author == last.author
                    && row.kind == last.kind
                    && (last.at - row.at).whole_seconds() < 300
                    && !names.contains_key(&row.seq)
            });
            if !join {
                if groups.len() == limit {
                    break 'pages;
                }
                groups.push(Vec::new());
            }
            groups.last_mut().expect("group").push(row);
        }
        if done {
            break;
        }
    }
    let accounts = state
        .accounts
        .get_map(groups.iter().filter_map(|g| g.first().map(|r| r.author)))
        .await;
    let mut versions = Vec::new();
    for group in &groups {
        let first = &group[0];
        let last = group.last().expect("group");
        let account = view::account_json(accounts.get(&first.author), first.author);
        let count: i64 = group.iter().map(|op| op.cells_changed).sum();
        let summary = if group.len() == 1 {
            first.summary.clone()
        } else {
            format!(
                "{} changed {} cells in {} operations",
                account["id"].as_str().unwrap_or("An account"),
                count,
                group.len()
            )
        };
        versions.push(
            json!({"version": first.seq, "from_version": last.seq, "by": account,
            "started_at": crate::db::rfc3339(last.at), "ended_at": crate::db::rfc3339(first.at),
            "summary": summary, "name": names.get(&first.seq), "kind": first.kind, "facts": first.facts, "undoes": first.undoes}),
        );
    }
    let next = groups
        .last()
        .and_then(|g| g.last())
        .map(|r| r.seq)
        .unwrap_or(before);
    let has_more = sqlx::query_scalar::<_, bool>(
        "select exists(select 1 from ops where line_id = $1 and seq < $2)",
    )
    .bind(&ctx.line.id)
    .bind(next)
    .fetch_one(&state.db)
    .await?;
    let include_creation = !has_more && next > 0 && groups.len() < limit;
    if include_creation {
        let (author, at): (Uuid, time::OffsetDateTime) =
            sqlx::query_as("select created_by, created_at from lines where id = $1")
                .bind(&ctx.line.id)
                .fetch_one(&state.db)
                .await?;
        let accounts = state.accounts.get_map([author]).await;
        versions.push(json!({"version": 0, "from_version": 0, "by": view::account_json(accounts.get(&author), author), "started_at": crate::db::rfc3339(at), "ended_at": crate::db::rfc3339(at), "summary": "Created the sheet", "name": names.get(&0), "kind": "create"}));
    }
    Ok(Json(
        json!({"versions": versions, "next_cursor": (has_more || (next > 0 && !include_creation)).then(|| next.to_string())}),
    ))
}

#[derive(Deserialize)]
struct NameBody {
    name: String,
}
async fn name(
    State(state): State<AppState>,
    caller: Caller,
    Path(p): Path<VersionPath>,
    JsonBody(body): JsonBody<NameBody>,
) -> ApiResult<Json<Value>> {
    let ctx = LineCtx::load(&state, Some(&caller), &p.sheet, p.copy.as_deref()).await?;
    ctx.require_edit()?;
    let v = version_number(&p.v)?;
    if v > ctx.line.head {
        return Err(no_such_version(v, ctx.line.head));
    }
    let name = body.name.trim();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(ApiError::invalid_field(
            "name",
            "A version name needs 1 to 200 characters.",
        ));
    }
    sqlx::query("insert into version_names (line_id, seq, name, named_by) values ($1,$2,$3,$4) on conflict (line_id,seq) do update set name=excluded.name,named_by=excluded.named_by,named_at=now()")
        .bind(&ctx.line.id).bind(v).bind(name).bind(caller.uuid).execute(&state.db).await?;
    Ok(Json(json!({"version": v, "name": name})))
}
async fn unname(
    State(state): State<AppState>,
    caller: Caller,
    Path(p): Path<VersionPath>,
) -> ApiResult<StatusCode> {
    let ctx = LineCtx::load(&state, Some(&caller), &p.sheet, p.copy.as_deref()).await?;
    ctx.require_edit()?;
    let v = version_number(&p.v)?;
    if v > ctx.line.head {
        return Err(no_such_version(v, ctx.line.head));
    }
    sqlx::query("delete from version_names where line_id=$1 and seq=$2")
        .bind(&ctx.line.id)
        .bind(v)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn tabs(
    State(state): State<AppState>,
    MaybeCaller(caller): MaybeCaller,
    Path(p): Path<VersionPath>,
) -> ApiResult<Json<Value>> {
    let ctx = LineCtx::load(&state, caller.as_ref(), &p.sheet, p.copy.as_deref()).await?;
    let v = version_number(&p.v)?;
    let src = Source::At(state_at(&state, &ctx.line.id, v).await?, v);
    Ok(Json(routes::tabs_answer(&state, &src).await?))
}
async fn cells(
    State(state): State<AppState>,
    MaybeCaller(caller): MaybeCaller,
    Path(p): Path<VersionTabPath>,
    headers: HeaderMap,
    QueryParams(q): QueryParams<CellsQuery>,
) -> ApiResult<Response> {
    let ctx = LineCtx::load(&state, caller.as_ref(), &p.sheet, p.copy.as_deref()).await?;
    let v = version_number(&p.v)?;
    let src = Source::At(state_at(&state, &ctx.line.id, v).await?, v);
    routes::cells_answer(&state, &src, view::TabRef::parse(&p.tab), &q, &headers).await
}

#[derive(Deserialize, Default)]
pub(crate) struct CompareQuery {
    pub from: Option<String>,
    pub to: Option<String>,
    pub limit: Option<usize>,
    pub tab: Option<String>,
    pub range: Option<String>,
}
async fn compare(
    State(state): State<AppState>,
    MaybeCaller(caller): MaybeCaller,
    Path(p): Path<LinePath>,
    QueryParams(q): QueryParams<CompareQuery>,
) -> ApiResult<Json<Value>> {
    let ctx = LineCtx::of(&state, caller.as_ref(), &p).await?;
    if ctx.copy_id().is_some() && q.from.is_none() {
        return super::copies::compare(&state, &ctx, q).await.map(Json);
    }
    let from = q
        .from
        .as_deref()
        .ok_or_else(|| ApiError::invalid_field("from", "Give the first version to compare."))?;
    let from = version_number(from)?;
    let to = routes::parse_version(q.to.as_deref(), "to")?
        .unwrap_or(head_of(&state, &ctx.line.id).await?);
    let a = state_at(&state, &ctx.line.id, from).await?;
    let b = state_at(&state, &ctx.line.id, to).await?;
    Ok(Json(diff_answer(&state, a, b, q).await?))
}

pub(crate) async fn diff_answer(
    state: &AppState,
    before: Arc<Workbook>,
    after: Arc<Workbook>,
    q: CompareQuery,
) -> ApiResult<Value> {
    let (mut answer, uuids) = blocking(move || -> ApiResult<(Value, Vec<Uuid>)> {
        let only = match q.tab {
            Some(t) => {
                let t = view::find_tab(&after, &view::TabRef::parse(&t))
                    .or_else(|_| view::find_tab(&before, &view::TabRef::parse(&t)))?;
                let rect = q
                    .range
                    .as_deref()
                    .map(|r| view::parse_range(r, "range").map(|w| w.rect))
                    .transpose()?
                    .unwrap_or(calc_engine::Rect::new(
                        0,
                        0,
                        calc_engine::addr::MAX_ROWS - 1,
                        calc_engine::addr::MAX_COLS - 1,
                    ));
                Some((t.id, rect))
            }
            None if q.range.is_some() => {
                return Err(ApiError::invalid_field(
                    "tab",
                    "Choose a tab when comparing a range.",
                ));
            }
            None => None,
        };
        let diff =
            calc_engine::diff::diff(&before, &after, q.limit.unwrap_or(500).min(10000), only);
        let mut value = serde_json::to_value(&diff).map_err(ApiError::internal)?;
        let mut uuids = Vec::new();
        for (cell, json) in diff
            .cells
            .iter()
            .zip(value["cells"].as_array_mut().expect("cells"))
        {
            let author = after
                .tab(cell.tab)
                .and_then(|tab| after.cell_author(tab.cell_by_id(cell.row, cell.col).meta))
                .and_then(|(id, _)| Uuid::parse_str(&id).ok());
            json["a"] = json!(cell.address);
            json["by"] = json!(author);
            if let Some(u) = author {
                uuids.push(u);
            }
        }
        Ok((value, uuids))
    })
    .await??;
    let (map, accounts) = view::accounts_map(state, uuids).await;
    if let Some(cells) = answer["cells"].as_array_mut() {
        for cell in cells {
            if let Some(u) = cell["by"].as_str().and_then(|s| Uuid::parse_str(s).ok()) {
                cell["by"] = view::account_json(accounts.get(&u), u);
            }
        }
    }
    answer["accounts"] = map;
    Ok(answer)
}
