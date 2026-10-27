//! Forked lines and three-way merges into their parent.

use super::{
    LineEvent, OpKind,
    ctx::{LineCtx, LinePath, both},
    task::{Job, blocking},
    versions, view,
};
use crate::{
    auth::{Caller, MaybeCaller},
    error::{ApiError, ApiResult, JsonBody, QueryParams},
    state::AppState,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, patch, post},
};
use calc_engine::{
    Workbook,
    diff::{MergeChoice, MergePlan, Side},
    merge_ids::MergeIds,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

pub(crate) fn router() -> Router<AppState> {
    let r = Router::new().route("/sheets/{sheet}/copies", get(list));
    let r = both(r, "/copies", post(create));
    r.route(
        "/sheets/{sheet}/copies/{copy}",
        patch(rename).delete(delete),
    )
    .route("/sheets/{sheet}/copies/{copy}/merge", post(merge))
}

#[derive(sqlx::FromRow)]
struct CopyRow {
    id: String,
    sheet_id: String,
    name: Option<String>,
    parent_id: Option<String>,
    forked_at: Option<i64>,
    head: i64,
    created_by: Uuid,
    created_at: time::OffsetDateTime,
    status: String,
    merged_at: Option<time::OffsetDateTime>,
    merged_by: Option<Uuid>,
}
async fn render(state: &AppState, row: CopyRow) -> Value {
    let accounts = state
        .accounts
        .get_map(std::iter::once(row.created_by).chain(row.merged_by))
        .await;
    json!({"id": row.id, "sheet_id": row.sheet_id, "name": row.name,
        "parent_id": row.parent_id.filter(|id| !id.starts_with("main:")), "forked_at": row.forked_at,
        "version": row.head, "created_by": view::account_json(accounts.get(&row.created_by),row.created_by),
        "created_at": crate::db::rfc3339(row.created_at), "status": row.status,
        "merged_at": row.merged_at.map(crate::db::rfc3339), "merged_by": row.merged_by.map(|u| view::account_json(accounts.get(&u),u))})
}
async fn answer(state: &AppState, id: &str) -> ApiResult<Value> {
    let row = sqlx::query_as::<_,CopyRow>("select id,sheet_id,name,parent_id,forked_at,head,created_by,created_at,status,merged_at,merged_by from lines where id=$1 and not is_main")
        .bind(id).fetch_one(&state.db).await?;
    Ok(render(state, row).await)
}
async fn publish(state: &AppState, ctx: &LineCtx, copy: Value) -> ApiResult<()> {
    let event = LineEvent::Copy(json!({"copy":copy}));
    ctx.handle(state).await?.publish(event.clone());
    if let Some(parent) = &ctx.line.parent_id {
        state.lines.open(state, parent).await?.publish(event);
    }
    Ok(())
}
#[derive(Deserialize)]
struct ListQuery {
    status: Option<String>,
}
async fn list(
    State(state): State<AppState>,
    MaybeCaller(caller): MaybeCaller,
    Path(p): Path<LinePath>,
    QueryParams(q): QueryParams<ListQuery>,
) -> ApiResult<Json<Value>> {
    let ctx = LineCtx::of(&state, caller.as_ref(), &p).await?;
    let all = match q.status.as_deref().unwrap_or("open") {
        "all" => true,
        "open" => false,
        _ => return Err(ApiError::invalid_field("status", "Choose open or all.")),
    };
    let rows = sqlx::query_as::<_,CopyRow>("select id,sheet_id,name,parent_id,forked_at,head,created_by,created_at,status,merged_at,merged_by from lines where sheet_id=$1 and not is_main and ($2 or status='open') order by created_at,id")
        .bind(&ctx.line.sheet_id).bind(all).fetch_all(&state.db).await?;
    let mut copies = Vec::new();
    for row in rows {
        copies.push(render(&state, row).await);
    }
    Ok(Json(json!({"copies":copies})))
}
#[derive(Deserialize)]
struct NameBody {
    name: String,
}
fn clean_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(ApiError::invalid_field(
            "name",
            "A copy name needs 1 to 200 characters.",
        ));
    }
    Ok(name.into())
}
async fn create(
    State(state): State<AppState>,
    caller: Caller,
    Path(p): Path<LinePath>,
    JsonBody(body): JsonBody<NameBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let ctx = LineCtx::of(&state, Some(&caller), &p).await?;
    ctx.require_edit()?;
    let name = clean_name(&body.name)?;
    let handle = ctx.handle(&state).await?;
    let version = handle.shared.snapshot().await?;
    let (blob, bytes): (String, i64) =
        sqlx::query_as("select blob,bytes from snapshots where line_id=$1 and seq=$2")
            .bind(&ctx.line.id)
            .bind(version)
            .fetch_one(&state.db)
            .await?;
    let id = crate::ids::copy();
    let mut tx = state.db.begin().await?;
    sqlx::query("insert into lines (id,sheet_id,is_main,name,parent_id,forked_at,created_by) values ($1,$2,false,$3,$4,$5,$6)")
        .bind(&id).bind(&p.sheet).bind(name).bind(&ctx.line.id).bind(version).bind(caller.uuid).execute(&mut *tx).await?;
    sqlx::query("insert into snapshots (line_id,seq,blob,bytes) values ($1,0,$2,$3)")
        .bind(&id)
        .bind(blob)
        .bind(bytes)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let copy = answer(&state, &id).await?;
    handle.publish(LineEvent::Copy(json!({"copy":copy})));
    Ok((StatusCode::CREATED, Json(copy)))
}
async fn rename(
    State(state): State<AppState>,
    caller: Caller,
    Path(p): Path<LinePath>,
    JsonBody(body): JsonBody<NameBody>,
) -> ApiResult<Json<Value>> {
    let ctx = LineCtx::of(&state, Some(&caller), &p).await?;
    ctx.require_edit()?;
    sqlx::query("update lines set name=$2 where id=$1 and status='open'")
        .bind(&ctx.line.id)
        .bind(clean_name(&body.name)?)
        .execute(&state.db)
        .await?;
    let copy = answer(&state, &ctx.line.id).await?;
    publish(&state, &ctx, copy.clone()).await?;
    Ok(Json(copy))
}
async fn delete(
    State(state): State<AppState>,
    caller: Caller,
    Path(p): Path<LinePath>,
) -> ApiResult<StatusCode> {
    let ctx = LineCtx::of(&state, Some(&caller), &p).await?;
    ctx.require_edit()?;
    sqlx::query("update lines set status='deleted',deleted_at=now(),deleted_by=$2 where id=$1 and status='open'").bind(&ctx.line.id).bind(caller.uuid).execute(&state.db).await?;
    publish(&state, &ctx, answer(&state, &ctx.line.id).await?).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn merge_error(e: calc_engine::diff::MergeError) -> ApiError {
    match e {
        calc_engine::diff::MergeError::Op(e) => e.into(),
    }
}
fn conflict_json(plan: &MergePlan) -> Vec<Value> {
    plan.conflicts.iter().map(|c| json!({"tab":c.tab,"tab_name":c.tab_name,"a":c.address,"base":c.base,"sheet":c.ours,"copy":c.theirs,"taken":if c.resolved==Side::Theirs {"copy"} else {"sheet"}})).collect()
}
fn plan_json(plan: &MergePlan, version: i64) -> Value {
    json!({"version":version,"cells_taken":plan.cells_taken,"conflicts":conflict_json(plan),"tabs_added":plan.tabs_added,"tabs_removed":plan.tabs_removed,"rows_added":plan.rows_added,"rows_removed":plan.rows_removed})
}
struct MergeStates {
    base: Arc<Workbook>,
    ours: Arc<Workbook>,
    theirs: Arc<Workbook>,
    parent_version: i64,
    copy_version: i64,
    receipt_revision: i64,
    accepted_parent_version: i64,
    ids: MergeIds,
}

async fn states(state: &AppState, ctx: &LineCtx) -> ApiResult<MergeStates> {
    let parent = ctx
        .line
        .parent_id
        .as_deref()
        .ok_or_else(|| ApiError::not_found("This copy's parent"))?;
    let receipt: Option<(String, Value, i64, i64)> = sqlx::query_as(
        "select source_blob,ids,parent_version,revision from merge_receipts where line_id=$1",
    )
    .bind(&ctx.line.id)
    .fetch_optional(&state.db)
    .await?;
    let (base, ids, accepted_parent_version, receipt_revision) =
        if let Some((blob, ids, parent_version, revision)) = receipt {
            let bytes = state.blobs.get(&blob).await?;
            let base = blocking(move || calc_engine::snapshot::decode(&bytes))
                .await?
                .map_err(|e| ApiError::internal(format!("reading merge baseline: {e}")))?;
            let ids = serde_json::from_value(ids)
                .map_err(|e| ApiError::internal(format!("reading merge identities: {e}")))?;
            (Arc::new(base), ids, parent_version, revision)
        } else {
            let base = versions::state_at(state, parent, ctx.line.forked_at.unwrap_or(0)).await?;
            let ids = identity_map(&base);
            (base, ids, ctx.line.forked_at.unwrap_or(0), 0)
        };
    let parent_handle = state.lines.open(state, parent).await?;
    let (ours, parent_version) = parent_handle
        .read(|wb, v| (Arc::new(wb.clone()), v))
        .await?;
    let (theirs, copy_version) = ctx
        .handle(state)
        .await?
        .read(|wb, v| (Arc::new(wb.clone()), v))
        .await?;
    Ok(MergeStates {
        base,
        ours,
        theirs,
        parent_version,
        copy_version,
        receipt_revision,
        accepted_parent_version,
        ids,
    })
}

fn identity_map(wb: &Workbook) -> MergeIds {
    let mut ids = MergeIds::default();
    for t in &wb.tabs {
        ids.tabs.insert(t.id, t.id);
        ids.rows
            .insert(t.id, t.rows.ids().iter().map(|&id| (id, id)).collect());
        ids.cols
            .insert(t.id, t.cols.ids().iter().map(|&id| (id, id)).collect());
    }
    ids
}

// Allocate temporary source identities outside the parent's namespace. The engine supplies their
// actual destinations after insertion; only those accepted mappings are saved with the baseline.
fn planning_ids(s: &MergeStates) -> MergeIds {
    let mut ids = s.ids.clone();
    let mut next_tab = s
        .ours
        .next_tab_id
        .max(ids.tabs.values().max().copied().unwrap_or(0) + 1);
    for t in s.base.tabs.iter().chain(&s.theirs.tabs) {
        let dest = *ids.tabs.entry(t.id).or_insert_with(|| {
            let id = next_tab;
            next_tab += 1;
            id
        });
        let parent = s.ours.tab(dest);
        for (maps, axis, parent_axis) in [
            (&mut ids.rows, &t.rows, parent.map(|t| &t.rows)),
            (&mut ids.cols, &t.cols, parent.map(|t| &t.cols)),
        ] {
            let map = maps.entry(t.id).or_default();
            let mut next = parent_axis
                .map_or(0, |a| a.next_id())
                .max(map.values().max().copied().map_or(0, |n| n + 1));
            for id in axis.ids() {
                map.entry(*id).or_insert_with(|| {
                    let id = next;
                    next += 1;
                    id
                });
            }
        }
    }
    ids
}

fn remap_choice(choice: &MergeChoice, ids: &MergeIds) -> MergeChoice {
    MergeChoice {
        tabs: choice.tabs.iter().map(|id| ids.tabs[id]).collect(),
        area: choice.area.map(|(id, area)| (ids.tabs[&id], area)),
        resolutions: choice
            .resolutions
            .iter()
            .map(|&((t, r, c), side)| ((ids.tabs[&t], ids.rows[&t][&r], ids.cols[&t][&c]), side))
            .collect(),
        default: choice.default,
    }
}

fn make_plan(s: &MergeStates, choice: &MergeChoice) -> ApiResult<(MergePlan, MergeIds)> {
    let temporary = planning_ids(s);
    let base = calc_engine::merge_ids::remap(&s.base, &temporary)?;
    let theirs = calc_engine::merge_ids::remap(&s.theirs, &temporary)?;
    let mut plan =
        calc_engine::diff::plan_merge(&base, &s.ours, &theirs, &remap_choice(choice, &temporary))
            .map_err(merge_error)?;
    let inverse_tabs: std::collections::BTreeMap<_, _> =
        temporary.tabs.iter().map(|(&a, &b)| (b, a)).collect();
    let mut accepted = s.ids.clone();
    for (&from, &dest) in &plan.ids.tabs {
        let source = inverse_tabs[&from];
        accepted.tabs.insert(source, dest);
        for (saved, temporary_axis, applied) in [
            (&mut accepted.rows, &temporary.rows, &plan.ids.rows),
            (&mut accepted.cols, &temporary.cols, &plan.ids.cols),
        ] {
            if let Some(applied) = applied.get(&from) {
                let inverse: std::collections::BTreeMap<_, _> = temporary_axis[&source]
                    .iter()
                    .map(|(&a, &b)| (b, a))
                    .collect();
                let saved = saved.entry(source).or_default();
                for (&from, &dest) in applied {
                    saved.insert(inverse[&from], dest);
                }
            }
        }
    }
    for conflict in &mut plan.conflicts {
        let mapped = theirs.tab(conflict.tab).expect("conflict source tab");
        conflict.tab = inverse_tabs[&conflict.tab];
        let original = s.theirs.tab(conflict.tab).expect("original source tab");
        conflict.row = mapped
            .rows
            .pos_of(conflict.row)
            .and_then(|pos| original.rows.id_at(pos))
            .unwrap_or(conflict.row);
        conflict.col = mapped
            .cols
            .pos_of(conflict.col)
            .and_then(|pos| original.cols.id_at(pos))
            .unwrap_or(conflict.col);
    }
    Ok((plan, accepted))
}

pub(crate) async fn compare(
    state: &AppState,
    ctx: &LineCtx,
    q: versions::CompareQuery,
) -> ApiResult<Value> {
    let states = states(state, ctx).await?;
    let diff = versions::diff_answer(state, states.base.clone(), states.theirs.clone(), q).await?;
    let parent_changed = states.parent_version != states.accepted_parent_version;
    let (plan, _) = blocking(move || make_plan(&states, &MergeChoice::default())).await??;
    Ok(json!({"diff":diff,"conflicts":conflict_json(&plan),"parent_changed":parent_changed}))
}
#[derive(Deserialize, Default)]
struct MergeBody {
    #[serde(default)]
    tabs: Vec<u32>,
    range: Option<RangeBody>,
    #[serde(default)]
    resolutions: Vec<Resolution>,
    default: Option<String>,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    close: bool,
}
#[derive(Deserialize)]
struct RangeBody {
    tab: view::TabRef,
    range: String,
}
#[derive(Deserialize)]
struct Resolution {
    tab: view::TabRef,
    a: String,
    take: String,
}
fn side(s: &str) -> ApiResult<Side> {
    match s {
        "copy" => Ok(Side::Theirs),
        "sheet" => Ok(Side::Ours),
        _ => Err(ApiError::invalid_field("take", "Choose copy or sheet.")),
    }
}
async fn merge(
    State(state): State<AppState>,
    caller: Caller,
    Path(p): Path<LinePath>,
    JsonBody(body): JsonBody<MergeBody>,
) -> ApiResult<Json<Value>> {
    let ctx = LineCtx::of(&state, Some(&caller), &p).await?;
    ctx.require_edit()?;
    let parent_id = ctx
        .line
        .parent_id
        .clone()
        .ok_or_else(|| ApiError::not_found("This copy's parent"))?;
    let parent = super::line_for(
        &state,
        &p.sheet,
        if parent_id.starts_with("main:") {
            None
        } else {
            Some(&parent_id)
        },
    )
    .await?;
    if parent.status != "open" {
        return Err(ApiError::conflict(
            "This copy's parent is closed.",
            "Make a copy of an open line to continue.",
        ));
    }
    let states = states(&state, &ctx).await?;
    let v = states.parent_version;
    let copy_version = states.copy_version;
    let receipt_revision = states.receipt_revision;
    let theirs = states.theirs.clone();
    let mut choice = MergeChoice {
        tabs: body.tabs,
        default: side(body.default.as_deref().unwrap_or("copy"))?,
        ..Default::default()
    };
    for tab in &choice.tabs {
        view::find_tab(&theirs, &view::TabRef::Id(*tab))?;
    }
    if let Some(r) = body.range {
        let t = view::find_tab(&theirs, &r.tab)?;
        choice.area = Some((t.id, view::parse_range(&r.range, "range")?.rect));
    }
    for r in body.resolutions {
        let t = view::find_tab(&theirs, &r.tab)?;
        let a = view::parse_cell(&r.a, "a")?;
        let ids = t
            .rows
            .id_at(a.row)
            .zip(t.cols.id_at(a.col))
            .ok_or_else(|| {
                ApiError::invalid_field("a", "This cell is outside the copy's used grid.")
            })?;
        choice
            .resolutions
            .push(((t.id, ids.0, ids.1), side(&r.take)?));
    }
    let dry_run = body.dry_run;
    let (mut plan, receipt) = blocking(move || -> ApiResult<_> {
        let (plan, ids) = make_plan(&states, &choice)?;
        let receipt = if dry_run {
            None
        } else {
            let accepted =
                calc_engine::merge_ids::accept_merge(&states.base, &states.theirs, &choice)?;
            Some((
                calc_engine::snapshot::encode(&accepted),
                serde_json::to_value(ids).map_err(|e| ApiError::internal(e.to_string()))?,
            ))
        };
        Ok((plan, receipt))
    })
    .await??;
    let mut result = plan_json(&plan, v);
    if body.dry_run {
        return Ok(Json(result));
    }
    let (receipt_bytes, receipt_ids) = receipt.expect("non-dry-run receipt");
    let receipt_blob = state.blobs.put(&receipt_bytes).await?;
    let parent_handle = state.lines.open(&state, &parent_id).await?;
    let id = ctx.line.id.clone();
    let by = caller.uuid;
    let close = body.close;
    let owner = ctx.access.sheet.owner;
    let data = json!({"sheet":{"id":p.sheet,"title":ctx.access.sheet.title},"copy":{"id":id,"name":ctx.line.name},"url":state.cfg.sheet_url(&p.sheet),"cli":format!("calc copies list {}",p.sheet)});
    let has_changes = plan.op.is_some();
    let op = plan.op.take();
    let mut job = Job::new(by, OpKind::Merge, vec![Box::new(move |_| Ok(op))]);
    job.expect_version = Some(v);
    job.summary = Some(format!(
        "Merged copy \"{}\"",
        ctx.line.name.as_deref().unwrap_or("")
    ));
    job.hook = Some(Box::new(move |conn, seq| {
        Box::pin(async move {
            let updated = sqlx::query("update lines set merged_at=now(),merged_by=$2,status=case when $3 then 'merged' else status end where id=$1 and status='open' and head=$4")
            .bind(&id).bind(by).bind(close).bind(copy_version).execute(&mut *conn).await?;
            if updated.rows_affected() != 1 {
                return Err(ApiError::conflict(
                    "The copy changed or closed while its merge was prepared.",
                    "Compare it again, then retry the merge.",
                ));
            }
            let saved = sqlx::query("insert into merge_receipts (line_id,source_blob,ids,parent_version,copy_version,revision) values ($1,$2,$3,$4,$5,1) on conflict (line_id) do update set source_blob=excluded.source_blob,ids=excluded.ids,parent_version=excluded.parent_version,copy_version=excluded.copy_version,revision=merge_receipts.revision+1 where merge_receipts.revision=$6")
                .bind(&id).bind(receipt_blob).bind(receipt_ids).bind(seq).bind(copy_version).bind(receipt_revision).execute(&mut *conn).await?;
            if saved.rows_affected() != 1 {
                return Err(ApiError::conflict(
                    "Another merge completed while this merge was prepared.",
                    "Compare the copy again and retry.",
                ));
            }
            if owner != by && has_changes {
                crate::notify::enqueue(
                    conn,
                    "calc.copy.merged",
                    owner,
                    &format!("copy-merge:{id}:{seq}"),
                    data,
                )
                .await?;
            }
            Ok(())
        })
    }));
    result["version"] = json!(parent_handle.shared.run(job).await?.version);
    publish(&state, &ctx, answer(&state, &ctx.line.id).await?).await?;
    Ok(Json(result))
}
