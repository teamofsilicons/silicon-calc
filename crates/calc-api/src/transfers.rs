//! Transfers: the owner of a sheet or folder hands it to another Carbon or Silicon, who has to accept before it
//! moves. See docs/API.md, Sheets and folders.
//!
//! On acceptance the new owner owns the sheet, or the folder with everything in it, at their top level; the
//! previous owner stays on it as an editor. Something in the previous owner's trash inside a transferred folder
//! stays theirs (at their top level, still in the trash). One transfer per item waits at a time: starting a new one
//! cancels the old.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    accounts::Account,
    auth::Caller,
    db::{rfc3339, rfc3339_opt},
    error::{ApiError, ApiResult, JsonBody},
    folders, ids,
    perms::{self, Role},
    sharing::Kind,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/sheets/{sheet}/transfer", post(transfer_sheet))
        .route("/folders/{folder}/transfer", post(transfer_folder))
        .route("/transfers", get(list))
        .route("/transfers/{id}/accept", post(accept))
        .route("/transfers/{id}/decline", post(decline))
        .route("/transfers/{id}/cancel", post(cancel))
}

#[derive(Clone, Debug, sqlx::FromRow)]
struct TransferRow {
    id: String,
    item_kind: String,
    item_id: String,
    from_account: Uuid,
    to_account: Uuid,
    status: String,
    created_at: OffsetDateTime,
    decided_at: Option<OffsetDateTime>,
    title: String,
}

const COLUMNS: &str = "id, item_kind, item_id, from_account, to_account, status, created_at, decided_at, title";

async fn render(state: &AppState, t: &TransferRow) -> ApiResult<Value> {
    // The item's name now, or what it was called when the transfer started.
    let current: Option<(String,)> = match t.item_kind.as_str() {
        "sheet" => sqlx::query_as("select title from sheets where id = $1 and deleted_at is null"),
        _ => sqlx::query_as("select name from folders where id = $1 and deleted_at is null"),
    }
    .bind(&t.item_id)
    .fetch_optional(&state.db)
    .await?;
    let accounts = state.accounts.get_map([t.from_account, t.to_account]).await;
    let acct = |u: Uuid| accounts.get(&u).cloned().unwrap_or_else(|| Account::unknown(u));
    Ok(json!({
        "id": t.id,
        "kind": t.item_kind,
        "item_id": t.item_id,
        "title": current.map(|c| c.0).unwrap_or_else(|| t.title.clone()),
        "from": acct(t.from_account),
        "to": acct(t.to_account),
        "status": t.status,
        "created_at": rfc3339(t.created_at),
        "decided_at": rfc3339_opt(t.decided_at),
    }))
}

async fn load(state: &AppState, id: &str) -> ApiResult<Option<TransferRow>> {
    if !ids::looks_like_id(id) {
        return Ok(None);
    }
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!("select {COLUMNS} from transfers where id = $1")))
        .bind(id)
        .fetch_optional(&state.db)
        .await?)
}

#[derive(Deserialize)]
struct TransferBody {
    to: Option<String>,
}

async fn start(state: &AppState, caller: &Caller, kind: Kind, item_id: &str, title: &str, to: Option<&str>) -> ApiResult<(StatusCode, Json<Value>)> {
    let Some(to) = to.map(str::trim).filter(|t| !t.is_empty()) else {
        return Err(ApiError::invalid_field("to", "Give who it goes to, as a c:id or si:id."));
    };
    let lower = to.to_ascii_lowercase();
    if !(lower.starts_with("c:") || lower.starts_with("si:")) {
        return Err(ApiError::invalid_field("to", format!("{to} isn't a c:id or si:id; transfers go to an account by its id.")));
    }
    let Some(target) = state.accounts.by_public_id(&lower).await? else {
        return Err(ApiError::invalid_field("to", format!("No Carbon or Silicon has the id {to}."))
            .with_hint("Check the id with calc accounts lookup."));
    };
    if target.uuid == caller.uuid {
        return Err(ApiError::invalid_field("to", "You already own it, so there is no one to transfer it to."));
    }
    let id = ids::transfer();
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "update transfers set status = 'cancelled', decided_at = now()
         where item_kind = $1 and item_id = $2 and status = 'pending'",
    )
    .bind(kind.as_str())
    .bind(item_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "insert into transfers (id, item_kind, item_id, from_account, to_account, title) values ($1, $2, $3, $4, $5, $6)",
    )
    .bind(&id)
    .bind(kind.as_str())
    .bind(item_id)
    .bind(caller.uuid)
    .bind(target.uuid)
    .bind(title)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    let row = load(state, &id).await?.ok_or_else(|| ApiError::internal("a new transfer went missing"))?;
    Ok((StatusCode::CREATED, Json(render(state, &row).await?)))
}

async fn transfer_sheet(State(state): State<AppState>, caller: Caller, Path(sheet): Path<String>, JsonBody(body): JsonBody<TransferBody>) -> ApiResult<(StatusCode, Json<Value>)> {
    let access = perms::sheet_access(&state, Some(&caller), &sheet).await?;
    if access.role != Role::Owner {
        return Err(ApiError::not_allowed(
            format!("Only the owner of \"{}\" can transfer it.", access.sheet.title),
            "Ask its owner.",
        ));
    }
    access.require_live()?;
    start(&state, &caller, Kind::Sheet, &access.sheet.id, &access.sheet.title, body.to.as_deref()).await
}

async fn transfer_folder(State(state): State<AppState>, caller: Caller, Path(folder): Path<String>, JsonBody(body): JsonBody<TransferBody>) -> ApiResult<(StatusCode, Json<Value>)> {
    let access = perms::folder_access(&state, &caller, &folder).await?;
    if access.role != Role::Owner {
        return Err(ApiError::not_allowed(
            format!("Only the owner of the folder \"{}\" can transfer it.", access.folder.name),
            "Ask its owner.",
        ));
    }
    access.require_live()?;
    start(&state, &caller, Kind::Folder, &access.folder.id, &access.folder.name, body.to.as_deref()).await
}

async fn list(State(state): State<AppState>, caller: Caller) -> ApiResult<Json<Value>> {
    let rows: Vec<TransferRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select {COLUMNS} from transfers where to_account = $1 or from_account = $1
         order by (status = 'pending') desc, created_at desc limit 400"
    )))
    .bind(caller.uuid)
    .fetch_all(&state.db)
    .await?;
    let mut incoming = Vec::new();
    let mut outgoing = Vec::new();
    for r in &rows {
        let v = render(&state, r).await?;
        if r.to_account == caller.uuid {
            incoming.push(v);
        } else {
            outgoing.push(v);
        }
    }
    Ok(Json(json!({"incoming": incoming, "outgoing": outgoing})))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Decision {
    Accept,
    Decline,
    Cancel,
}

fn already(t: &TransferRow) -> ApiError {
    ApiError::conflict(
        format!("This transfer of \"{}\" was already {}.", t.title, t.status),
        "Nothing changed. Start a new transfer if it should still move.",
    )
}

async fn decide(state: AppState, caller: Caller, id: String, decision: Decision) -> ApiResult<Json<Value>> {
    let Some(t) = load(&state, &id).await? else { return Err(ApiError::not_found("This transfer")) };
    if t.to_account != caller.uuid && t.from_account != caller.uuid {
        return Err(ApiError::not_found("This transfer"));
    }
    match decision {
        Decision::Accept | Decision::Decline if t.to_account != caller.uuid => {
            let to = state.accounts.get(t.to_account).await;
            return Err(ApiError::not_allowed(
                format!("Only {} can accept or decline this transfer of \"{}\".", to.id, t.title),
                "To call it off, cancel it instead.",
            ));
        }
        Decision::Cancel if t.from_account != caller.uuid => {
            return Err(ApiError::not_allowed(
                format!("Only who started this transfer of \"{}\" can cancel it.", t.title),
                "To turn it down, decline it instead.",
            ));
        }
        _ => {}
    }
    let mut tx = state.db.begin().await?;
    let status: Option<(String,)> = sqlx::query_as("select status from transfers where id = $1 for update")
        .bind(&t.id)
        .fetch_optional(&mut *tx)
        .await?;
    if status.map(|s| s.0).as_deref() != Some("pending") {
        let fresh = load(&state, &t.id).await?.unwrap_or(t);
        return Err(already(&fresh));
    }
    let new_status = match decision {
        Decision::Accept => {
            hand_over(&mut tx, &t).await?;
            "accepted"
        }
        Decision::Decline => "declined",
        Decision::Cancel => "cancelled",
    };
    sqlx::query("update transfers set status = $2, decided_at = now() where id = $1")
        .bind(&t.id)
        .bind(new_status)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let row = load(&state, &t.id).await?.ok_or_else(|| ApiError::internal("a transfer went missing"))?;
    Ok(Json(render(&state, &row).await?))
}

fn gone(t: &TransferRow) -> ApiError {
    ApiError::conflict(
        format!("\"{}\" was deleted forever, so it can't be transferred.", t.title),
        "Decline the transfer; there is nothing left to take.",
    )
}

fn changed_owner(t: &TransferRow) -> ApiError {
    ApiError::conflict(
        format!("\"{}\" changed owner since this transfer started, so it can't go through.", t.title),
        "Ask its owner to start a new transfer.",
    )
}

fn in_trash(t: &TransferRow) -> ApiError {
    ApiError::conflict(
        format!("\"{}\" is in the trash, so it can't be transferred now.", t.title),
        "Ask its owner to restore it from the trash first; then accept again.",
    )
}

/// Moves ownership, inside the caller's transaction.
async fn hand_over(tx: &mut sqlx::PgConnection, t: &TransferRow) -> ApiResult<()> {
    let (from, to) = (t.from_account, t.to_account);
    match t.item_kind.as_str() {
        "sheet" => {
            let row: Option<(Uuid, Option<OffsetDateTime>, Option<String>)> = sqlx::query_as(
                "select owner, trashed_at, folder_id from sheets where id = $1 and deleted_at is null for update",
            )
            .bind(&t.item_id)
            .fetch_optional(&mut *tx)
            .await?;
            let Some((owner, trashed_at, folder_id)) = row else { return Err(gone(t)) };
            if owner != from {
                return Err(changed_owner(t));
            }
            if trashed_at.is_some() || folder_trashed(tx, folder_id.as_deref()).await? {
                return Err(in_trash(t));
            }
            sqlx::query("update sheets set owner = $2, folder_id = null, updated_at = now() where id = $1")
                .bind(&t.item_id)
                .bind(to)
                .execute(&mut *tx)
                .await?;
            keep_previous_owner(tx, "sheet", &t.item_id, from, to).await?;
            sqlx::query("delete from members where item_kind = 'sheet' and item_id = $1 and account = $2")
                .bind(&t.item_id)
                .bind(to)
                .execute(&mut *tx)
                .await?;
        }
        _ => {
            let row: Option<(Uuid, Option<OffsetDateTime>, Option<String>)> = sqlx::query_as(
                "select owner, trashed_at, parent_id from folders where id = $1 and deleted_at is null for update",
            )
            .bind(&t.item_id)
            .fetch_optional(&mut *tx)
            .await?;
            let Some((owner, trashed_at, parent_id)) = row else { return Err(gone(t)) };
            if owner != from {
                return Err(changed_owner(t));
            }
            if trashed_at.is_some() || folder_trashed(tx, parent_id.as_deref()).await? {
                return Err(in_trash(t));
            }
            // What's in the trash on its own stays with the previous owner, at their top level.
            let (all_folders, all_sheets) = folders::tree(tx, &t.item_id, false).await?;
            let (live_folders, live_sheets) = folders::tree(tx, &t.item_id, true).await?;
            let binned_folders: Vec<String> = all_folders.into_iter().filter(|f| !live_folders.contains(f)).collect();
            let binned_sheets: Vec<String> = all_sheets.into_iter().filter(|s| !live_sheets.contains(s)).collect();
            sqlx::query(
                "update folders set parent_id = null where id = any($1) and parent_id <> all($2)",
            )
            .bind(&binned_folders)
            .bind(&binned_folders)
            .execute(&mut *tx)
            .await?;
            sqlx::query("update sheets set folder_id = null where id = any($1) and folder_id = any($2)")
                .bind(&binned_sheets)
                .bind(&live_folders)
                .execute(&mut *tx)
                .await?;
            sqlx::query("update folders set owner = $2, updated_at = now() where id = any($1)")
                .bind(&live_folders)
                .bind(to)
                .execute(&mut *tx)
                .await?;
            sqlx::query("update folders set parent_id = null where id = $1")
                .bind(&t.item_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("update sheets set owner = $2, updated_at = now() where id = any($1)")
                .bind(&live_sheets)
                .bind(to)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "delete from members where account = $1
                   and ((item_kind = 'folder' and item_id = any($2)) or (item_kind = 'sheet' and item_id = any($3)))",
            )
            .bind(to)
            .bind(&live_folders)
            .bind(&live_sheets)
            .execute(&mut *tx)
            .await?;
            keep_previous_owner(tx, "folder", &t.item_id, from, to).await?;
            // Transfers the previous owner started for things inside can't go through any more.
            sqlx::query(
                "update transfers set status = 'cancelled', decided_at = now()
                 where status = 'pending' and id <> $1
                   and ((item_kind = 'folder' and item_id = any($2)) or (item_kind = 'sheet' and item_id = any($3)))",
            )
            .bind(&t.id)
            .bind(&live_folders)
            .bind(&live_sheets)
            .execute(&mut *tx)
            .await?;
        }
    }
    Ok(())
}

async fn folder_trashed(tx: &mut sqlx::PgConnection, folder_id: Option<&str>) -> ApiResult<bool> {
    let Some(folder_id) = folder_id else { return Ok(false) };
    let n: (i64,) = sqlx::query_as(
        "with recursive chain(id, depth) as (
             select id, 0 from folders where id = $1
             union all
             select f.parent_id, c.depth + 1 from chain c join folders f on f.id = c.id
             where f.parent_id is not null and c.depth < 100
         ) select count(*) from chain c join folders f on f.id = c.id where f.trashed_at is not null",
    )
    .bind(folder_id)
    .fetch_one(&mut *tx)
    .await?;
    Ok(n.0 > 0)
}

/// The previous owner stays on as an editor.
async fn keep_previous_owner(tx: &mut sqlx::PgConnection, kind: &str, item_id: &str, from: Uuid, to: Uuid) -> ApiResult<()> {
    sqlx::query(
        "insert into members (item_kind, item_id, account, role, invited_by) values ($1, $2, $3, 'editor', $4)
         on conflict (item_kind, item_id, account) do update set role = 'editor'",
    )
    .bind(kind)
    .bind(item_id)
    .bind(from)
    .bind(to)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

async fn accept(State(state): State<AppState>, caller: Caller, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    decide(state, caller, id, Decision::Accept).await
}

async fn decline(State(state): State<AppState>, caller: Caller, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    decide(state, caller, id, Decision::Decline).await
}

async fn cancel(State(state): State<AppState>, caller: Caller, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    decide(state, caller, id, Decision::Cancel).await
}
