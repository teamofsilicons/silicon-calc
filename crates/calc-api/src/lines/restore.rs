//! Restoring old states and removing one account's changes without rewriting history.

use super::{
    OpKind,
    ctx::{LineCtx, VersionPath, both},
    task::{Job, blocking},
    versions, view,
};
use crate::{
    auth::Caller,
    error::{ApiError, ApiResult, JsonBody},
    state::AppState,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::post,
};
use serde::Deserialize;
use serde_json::{Value, json};

pub(crate) fn router() -> Router<AppState> {
    let r = both(Router::new(), "/versions/{v}/restore", post(restore));
    let r = both(r, "/versions/{v}/undo-account", post(undo_account));
    both(r, "/versions/{v}/new-sheet", post(new_sheet))
}
#[derive(Deserialize, Default)]
struct RestoreBody {
    tab: Option<view::TabRef>,
    range: Option<String>,
}
async fn restore(
    State(state): State<AppState>,
    caller: Caller,
    Path(p): Path<VersionPath>,
    JsonBody(body): JsonBody<RestoreBody>,
) -> ApiResult<Json<Value>> {
    let ctx = LineCtx::load(&state, Some(&caller), &p.sheet, p.copy.as_deref()).await?;
    ctx.require_edit()?;
    let v = versions::version_number(&p.v)?;
    let target = versions::state_at(&state, &ctx.line.id, v).await?;
    let only = match body.tab {
        Some(t) => Some((
            view::find_tab(&target, &t)?.id,
            body.range
                .as_deref()
                .map(|r| view::parse_range(r, "range"))
                .transpose()?,
        )),
        None if body.range.is_some() => {
            return Err(ApiError::invalid_field(
                "tab",
                "Choose a tab when restoring a range.",
            ));
        }
        None => None,
    };
    let mut job = Job::new(
        caller.uuid,
        OpKind::Restore,
        vec![Box::new(move |current| {
            let only = only.map(|(id, window)| {
                (
                    id,
                    window.and_then(|window| {
                        let a = current.tab(id).and_then(|t| t.used_rect());
                        let b = target.tab(id).and_then(|t| t.used_rect());
                        let used = match (a, b) {
                            (Some(a), Some(b)) => Some(calc_engine::Rect::new(
                                a.r1.min(b.r1),
                                a.c1.min(b.c1),
                                a.r2.max(b.r2),
                                a.c2.max(b.c2),
                            )),
                            (a, b) => a.or(b),
                        };
                        view::effective(Some(window), used)
                    }),
                )
            });
            if let Some((_, Some(area))) = only {
                if area.rows() as u64 * area.cols() as u64 > 200_000 {
                    return Err(ApiError::too_large(
                        "This restore covers more than 200,000 cells.",
                        "Restore a smaller range, a whole tab or the whole sheet.",
                    ));
                }
            }
            calc_engine::diff::plan_restore(current, &target, only).map_err(ApiError::from)
        })],
    );
    job.summary = Some(format!("Restored version {v}"));
    let outcome = ctx.handle(&state).await?.shared.run(job).await?;
    Ok(Json(json!({"version":outcome.version})))
}
#[derive(Deserialize)]
struct AccountBody {
    account: String,
}
async fn undo_account(
    State(state): State<AppState>,
    caller: Caller,
    Path(p): Path<VersionPath>,
    JsonBody(body): JsonBody<AccountBody>,
) -> ApiResult<Json<Value>> {
    let ctx = LineCtx::load(&state, Some(&caller), &p.sheet, p.copy.as_deref()).await?;
    ctx.require_edit()?;
    let v = versions::version_number(&p.v)?;
    let account = state
        .accounts
        .by_public_id(&body.account)
        .await?
        .ok_or_else(|| {
            ApiError::invalid_field(
                "account",
                format!("There is no account named {}.", body.account),
            )
        })?;
    let handle = ctx.handle(&state).await?;
    let (current, head) = handle.read(|wb, v| (wb.clone(), v)).await?;
    versions::state_at(&state, &ctx.line.id, v).await?;
    let changed: Vec<i64> = sqlx::query_scalar("select seq from ops where line_id=$1 and seq>$2 and seq<=$3 and author=$4 order by seq desc")
        .bind(&ctx.line.id).bind(v).bind(head).bind(account.uuid).fetch_all(&state.db).await?;
    let excluded = account.uuid.to_string();
    let protected = std::sync::Arc::new(foreign_cells(&current, &excluded));
    let mut rebuilt = current.clone();
    for seq in changed {
        let before = versions::state_at(&state, &ctx.line.id, seq - 1).await?;
        let after = versions::state_at(&state, &ctx.line.id, seq).await?;
        let protected = protected.clone();
        let excluded = excluded.clone();
        rebuilt =
            blocking(move || -> ApiResult<_> {
                let choice = calc_engine::diff::MergeChoice {
                    default: calc_engine::diff::Side::Ours,
                    ..Default::default()
                };
                let plan = calc_engine::diff::plan_merge(&after, &rebuilt, &before, &choice)
                    .map_err(|e| match e {
                        calc_engine::diff::MergeError::Op(e) => ApiError::from(e),
                    })?;
                if let Some(op) = plan
                    .op
                    .and_then(|op| preserve_foreign(op, &protected, &rebuilt))
                {
                    rebuilt.apply(
                        &op,
                        &calc_engine::ops::Actor {
                            account: &excluded,
                            at: 0,
                        },
                    )?;
                }
                // A retained row or tab may still contain the excluded account's cells.
                // Clear only those cells; the later edits needing that scaffold stay put.
                for tab in &after.tabs {
                    let Some(now) = rebuilt.tab(tab.id) else {
                        continue;
                    };
                    let old = before.tab(tab.id);
                    let cells: Vec<_> = tab
                        .occupied()
                        .into_iter()
                        .filter(|(r, c)| {
                            !protected.contains(&(tab.id, *r, *c))
                                && now.rows.pos_of(*r).is_some()
                                && now.cols.pos_of(*c).is_some()
                                && old.is_none_or(|t| {
                                    t.rows.pos_of(*r).is_none() || t.cols.pos_of(*c).is_none()
                                })
                        })
                        .map(|(row, col)| calc_engine::ops::CellWrite {
                            row,
                            col,
                            clear: true,
                            style: Some(Default::default()),
                            ..Default::default()
                        })
                        .collect();
                    if !cells.is_empty() {
                        rebuilt.apply(
                            &calc_engine::Op::SetCells { tab: tab.id, cells },
                            &calc_engine::ops::Actor {
                                account: &excluded,
                                at: 0,
                            },
                        )?;
                    }
                }
                Ok(rebuilt)
            })
            .await??;
    }
    let (op, cells_changed) = blocking(move || -> ApiResult<_> {
        let cells = calc_engine::diff::diff(&current, &rebuilt, 0, None).cells_changed;
        Ok((
            calc_engine::diff::plan_restore(&current, &rebuilt, None)?,
            cells,
        ))
    })
    .await??;
    let mut job = Job::new(caller.uuid, OpKind::Undo, vec![Box::new(move |_| Ok(op))]);
    job.expect_version = Some(head);
    job.summary = Some(format!("Undid {}'s changes after version {v}", account.id));
    let outcome = handle.shared.run(job).await?;
    Ok(Json(
        json!({"version":outcome.version,"cells_changed":cells_changed}),
    ))
}
#[derive(Deserialize)]
struct NewSheetBody {
    title: Option<String>,
}
async fn new_sheet(
    State(state): State<AppState>,
    caller: Caller,
    Path(p): Path<VersionPath>,
    JsonBody(body): JsonBody<NewSheetBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let ctx = LineCtx::load(&state, Some(&caller), &p.sheet, p.copy.as_deref()).await?;
    ctx.require_edit()?;
    let v = versions::version_number(&p.v)?;
    let wb = versions::state_at(&state, &ctx.line.id, v).await?;
    let access =
        crate::sheets::create_sheet(&state, &caller, body.title.as_deref(), None, &wb).await?;
    Ok((
        StatusCode::CREATED,
        Json(crate::sheets::sheet_json_full(&state, &access).await?),
    ))
}

/// Later edits keep their cells and the rows, columns and tabs those cells need.
fn foreign_cells(
    wb: &calc_engine::Workbook,
    excluded: &str,
) -> std::collections::HashSet<(u32, u32, u32)> {
    wb.tabs
        .iter()
        .flat_map(|tab| {
            tab.occupied().into_iter().filter_map(|(r, c)| {
                wb.cell_author(tab.cell_by_id(r, c).meta)
                    .filter(|(by, _)| by != excluded)
                    .map(|_| (tab.id, r, c))
            })
        })
        .collect()
}
fn preserve_foreign(
    op: calc_engine::Op,
    keep: &std::collections::HashSet<(u32, u32, u32)>,
    current: &calc_engine::Workbook,
) -> Option<calc_engine::Op> {
    use calc_engine::Op;
    Some(match op {
        Op::Batch { ops } => {
            let ops: Vec<_> = ops
                .into_iter()
                .filter_map(|op| preserve_foreign(op, keep, current))
                .collect();
            if ops.is_empty() {
                return None;
            }
            Op::Batch { ops }
        }
        Op::SetCells { tab, cells } => {
            let cells: Vec<_> = cells
                .into_iter()
                .filter(|c| !keep.contains(&(tab, c.row, c.col)))
                .collect();
            if cells.is_empty() {
                return None;
            }
            Op::SetCells { tab, cells }
        }
        Op::DeleteTab { tab } if keep.iter().any(|(t, _, _)| *t == tab) => return None,
        Op::DeleteRows { tab, ids } => {
            if current
                .tab(tab)
                .is_some_and(|t| ids.len() == t.rows.len() as usize)
                && keep.iter().any(|(t, _, _)| *t == tab)
            {
                return None;
            }
            let ids: Vec<_> = ids
                .into_iter()
                .filter(|r| !keep.iter().any(|(t, kr, _)| *t == tab && kr == r))
                .collect();
            if ids.is_empty() {
                return None;
            }
            Op::DeleteRows { tab, ids }
        }
        Op::DeleteCols { tab, ids } => {
            if current
                .tab(tab)
                .is_some_and(|t| ids.len() == t.cols.len() as usize)
                && keep.iter().any(|(t, _, _)| *t == tab)
            {
                return None;
            }
            let ids: Vec<_> = ids
                .into_iter()
                .filter(|c| !keep.iter().any(|(t, _, kc)| *t == tab && kc == c))
                .collect();
            if ids.is_empty() {
                return None;
            }
            Op::DeleteCols { tab, ids }
        }
        other => other,
    })
}
