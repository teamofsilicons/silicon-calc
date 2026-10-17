//! Folders: making, opening, renaming, moving, the trash and deleting forever. See docs/API.md.
//!
//! A folder belongs to one Carbon or Silicon, and so does everything in it: a sheet or folder made inside someone
//! else's shared folder belongs to that folder's owner. Sharing a folder shares everything in it, including what is
//! added later. A folder in the trash takes everything in it along; restoring it brings them back.

use std::collections::HashMap;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::{
    accounts::Account,
    auth::Caller,
    db::{rfc3339, rfc3339_opt},
    error::{ApiError, ApiResult, JsonBody, QueryParams},
    ids,
    perms::{self, FOLDER_COLUMNS, FolderAccess, FolderRow, Role, SHEET_COLUMNS, SheetRow},
    sheets,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/folders", post(create))
        .route("/folders/{folder}", axum::routing::get(get_folder).patch(patch_folder).delete(trash_folder))
        .route("/folders/{folder}/restore", post(restore_folder))
        .route("/folders/{folder}/forever", delete(delete_forever))
}

pub const MAX_NAME: usize = 200;

fn clean_name(name: Option<&str>) -> ApiResult<String> {
    let n = name.map(str::trim).unwrap_or("");
    if n.is_empty() {
        return Err(ApiError::invalid_field("name", "A folder needs a name."));
    }
    if n.chars().count() > MAX_NAME {
        return Err(ApiError::invalid_field("name", format!("A folder's name can be at most {MAX_NAME} characters.")));
    }
    if n.chars().any(|c| c.is_control()) {
        return Err(ApiError::invalid_field("name", "A folder's name can't hold line breaks or other control characters."));
    }
    Ok(n.to_owned())
}

fn render(state: &AppState, f: &FolderRow, role: Option<Role>, trashed_at: Option<time::OffsetDateTime>, accounts: &HashMap<Uuid, Account>) -> Value {
    let acct = |u: Uuid| accounts.get(&u).cloned().unwrap_or_else(|| Account::unknown(u));
    let mut v = json!({
        "id": f.id,
        "name": f.name,
        "parent_id": f.parent_id,
        "owner": acct(f.owner),
        "created_at": rfc3339(f.created_at),
        "created_by": acct(f.created_by),
        "trashed_at": rfc3339_opt(trashed_at),
        "url": state.cfg.folder_url(&f.id),
    });
    // A folder the caller can't open (named only as where someone's access comes from) has no role to show.
    if let Some(role) = role {
        v["role"] = json!(role.as_str());
    }
    v
}

/// docs/API.md `Folder` for a folder the caller can open.
pub async fn folder_json(state: &AppState, access: &FolderAccess) -> Value {
    let f = &access.folder;
    let trashed_at = if access.in_trash {
        perms::trashed_since(state, f.trashed_at, f.parent_id.as_deref()).await.unwrap_or(f.trashed_at)
    } else {
        None
    };
    let accounts = state.accounts.get_map([f.owner, f.created_by]).await;
    render(state, f, Some(access.role), trashed_at, &accounts)
}

/// docs/API.md `Folder` for each row `me` can open, in the same order; others are left out.
pub async fn folder_list_json(state: &AppState, me: Uuid, rows: &[FolderRow]) -> ApiResult<Vec<Value>> {
    let roles = perms::folder_roles(state, me, rows).await?;
    let accounts = state.accounts.get_map(rows.iter().flat_map(|f| [f.owner, f.created_by])).await;
    let mut out = Vec::new();
    for f in rows {
        let Some(&(role, trash)) = roles.get(&f.id) else { continue };
        let trashed_at = if trash { perms::trashed_since(state, f.trashed_at, f.parent_id.as_deref()).await? } else { None };
        out.push(render(state, f, Some(role), trashed_at, &accounts));
    }
    Ok(out)
}

/// docs/API.md `Folder` for a folder `me` may not be able to open (`role` is then null): for where access comes from.
pub async fn folder_ref_json(state: &AppState, me: Option<Uuid>, f: &FolderRow) -> ApiResult<Value> {
    let role = match me {
        Some(me) => perms::folder_roles(state, me, std::slice::from_ref(f)).await?.remove(&f.id).map(|r| r.0),
        None => None,
    };
    let accounts = state.accounts.get_map([f.owner, f.created_by]).await;
    Ok(render(state, f, role, f.trashed_at, &accounts))
}

/// Every live folder under `root` (itself included) and every live sheet in them. With `skip_trashed`, folders and
/// sheets in the trash on their own (and what is under them) are left out.
pub(crate) async fn tree(conn: &mut PgConnection, root: &str, skip_trashed: bool) -> ApiResult<(Vec<String>, Vec<String>)> {
    let folders: Vec<(String,)> = sqlx::query_as(
        "with recursive t(id, depth) as (
             select id, 0 from folders where id = $1 and deleted_at is null
             union
             select f.id, t.depth + 1 from folders f join t on f.parent_id = t.id
             where f.deleted_at is null and t.depth < 100 and (not $2 or f.trashed_at is null)
         ) select id from t",
    )
    .bind(root)
    .bind(skip_trashed)
    .fetch_all(&mut *conn)
    .await?;
    let folders: Vec<String> = folders.into_iter().map(|f| f.0).collect();
    let sheets: Vec<(String,)> = sqlx::query_as(
        "select id from sheets where folder_id = any($1) and deleted_at is null and (not $2 or trashed_at is null)",
    )
    .bind(&folders)
    .bind(skip_trashed)
    .fetch_all(&mut *conn)
    .await?;
    Ok((folders, sheets.into_iter().map(|s| s.0).collect()))
}

#[derive(Deserialize)]
struct NewFolder {
    name: Option<String>,
    parent_id: Option<String>,
}

async fn create(State(state): State<AppState>, caller: Caller, JsonBody(body): JsonBody<NewFolder>) -> ApiResult<(StatusCode, Json<Value>)> {
    let name = clean_name(body.name.as_deref())?;
    let (parent, owner) = sheets::placement(&state, &caller, body.parent_id.as_deref()).await.map_err(|e| {
        if e.code == "invalid_field" { e.with_details(json!({"fields": {"parent_id": "No such folder that you can open."}})) } else { e }
    })?;
    let mut created = None;
    for _ in 0..8 {
        let id = ids::folder();
        let res = sqlx::query(
            "insert into folders (id, name, parent_id, owner, created_by) values ($1, $2, $3, $4, $5)
             on conflict (id) do nothing",
        )
        .bind(&id)
        .bind(&name)
        .bind(&parent)
        .bind(owner)
        .bind(caller.uuid)
        .execute(&state.db)
        .await?;
        if res.rows_affected() == 1 {
            created = Some(id);
            break;
        }
    }
    let Some(id) = created else { return Err(ApiError::internal("eight new folder ids in a row were taken")) };
    let access = perms::folder_access(&state, &caller, &id).await?;
    Ok((StatusCode::CREATED, Json(folder_json(&state, &access).await)))
}

async fn get_folder(State(state): State<AppState>, caller: Caller, Path(folder): Path<String>) -> ApiResult<Json<Value>> {
    let access = perms::folder_access(&state, &caller, &folder).await?;
    let me = caller.uuid;
    // The way to it: the folders above that the caller can open, top first.
    let above = perms::ancestors(&state, access.folder.parent_id.as_deref()).await?;
    let roles = perms::folder_roles(&state, me, &above).await?;
    let mut path_rows: Vec<FolderRow> = above.into_iter().take_while(|f| roles.contains_key(&f.id)).collect();
    path_rows.reverse();
    let path = folder_list_json(&state, me, &path_rows).await?;
    let sheet_rows: Vec<SheetRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select {SHEET_COLUMNS} from sheets where folder_id = $1 and deleted_at is null and trashed_at is null
         order by updated_at desc, id desc"
    )))
    .bind(&access.folder.id)
    .fetch_all(&state.db)
    .await?;
    let folder_rows: Vec<FolderRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select {FOLDER_COLUMNS} from folders where parent_id = $1 and deleted_at is null and trashed_at is null
         order by lower(name), id"
    )))
    .bind(&access.folder.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({
        "folder": folder_json(&state, &access).await,
        "path": path,
        "sheets": sheets::sheet_list_json(&state, Some(me), &sheet_rows).await?,
        "folders": folder_list_json(&state, me, &folder_rows).await?,
    })))
}

#[derive(Deserialize)]
struct FolderPatch {
    name: Option<String>,
    /// `null` moves the folder to its owner's top level.
    #[serde(default, deserialize_with = "sheets::explicit_null")]
    parent_id: Option<Option<String>>,
}

async fn check_move(state: &AppState, caller: &Caller, access: &FolderAccess, target: Option<&str>) -> ApiResult<()> {
    let folder = &access.folder;
    if target == folder.parent_id.as_deref() {
        return Ok(());
    }
    match target {
        Some(t) => {
            if t == folder.id {
                return Err(ApiError::invalid_field("parent_id", "A folder can't go inside itself."));
            }
            let into = perms::folder_access(state, caller, t).await.map_err(|e| {
                if e.code == "not_found" {
                    ApiError::invalid_field("parent_id", format!("There is no folder {t} that you can open."))
                } else {
                    e
                }
            })?;
            into.require_edit()?;
            if perms::ancestors(state, Some(t)).await?.iter().any(|f| f.id == folder.id) {
                return Err(ApiError::invalid_field(
                    "parent_id",
                    format!("\"{}\" is inside \"{}\", so \"{}\" can't go in it.", into.folder.name, folder.name, folder.name),
                ));
            }
            if into.folder.owner != folder.owner {
                let accounts = state.accounts.get_map([folder.owner, into.folder.owner]).await;
                let show = |u: &Uuid| accounts.get(u).map(|a| a.id.clone()).unwrap_or_default();
                return Err(ApiError::conflict(
                    format!(
                        "The folder \"{}\" belongs to {} and \"{}\" belongs to {}, and a folder only holds what its owner owns.",
                        folder.name,
                        show(&folder.owner),
                        into.folder.name,
                        show(&into.folder.owner)
                    ),
                    format!("Transfer \"{}\" to {} first (calc transfer), then move it.", folder.name, show(&into.folder.owner)),
                ));
            }
        }
        None => {
            if access.role != Role::Owner {
                return Err(ApiError::not_allowed(
                    format!("Only the owner of the folder \"{}\" can move it to the top level.", folder.name),
                    "Move it into another folder you can edit, or ask its owner.",
                ));
            }
        }
    }
    if let Some(from) = &folder.parent_id {
        perms::folder_access(state, caller, from).await?.require_edit()?;
    }
    Ok(())
}

async fn patch_folder(
    State(state): State<AppState>,
    caller: Caller,
    Path(folder): Path<String>,
    JsonBody(body): JsonBody<FolderPatch>,
) -> ApiResult<Json<Value>> {
    let access = perms::folder_access(&state, &caller, &folder).await?;
    if body.name.is_none() && body.parent_id.is_none() {
        return Ok(Json(folder_json(&state, &access).await));
    }
    access.require_edit()?;
    let name = body.name.as_deref().map(|n| clean_name(Some(n))).transpose()?;
    let target: Option<Option<String>> = body
        .parent_id
        .map(|p| p.map(|p| p.trim().to_owned()).filter(|p| !p.is_empty() && p != "root"));
    if let Some(t) = &target {
        check_move(&state, &caller, &access, t.as_deref()).await?;
    }
    sqlx::query(
        "update folders set name = coalesce($2, name),
                            parent_id = case when $3 then $4 else parent_id end,
                            updated_at = now()
         where id = $1",
    )
    .bind(&access.folder.id)
    .bind(&name)
    .bind(target.is_some())
    .bind(target.flatten())
    .execute(&state.db)
    .await?;
    let access = perms::folder_access(&state, &caller, &folder).await?;
    Ok(Json(folder_json(&state, &access).await))
}

async fn trash_folder(State(state): State<AppState>, caller: Caller, Path(folder): Path<String>) -> ApiResult<StatusCode> {
    let access = perms::folder_access(&state, &caller, &folder).await?;
    if access.role != Role::Owner {
        return Err(ApiError::not_allowed(
            format!("Only the owner of the folder \"{}\" can move it to the trash.", access.folder.name),
            "Ask its owner, or leave the folder instead.",
        ));
    }
    sqlx::query("update folders set trashed_at = now(), trashed_by = $2 where id = $1 and trashed_at is null")
        .bind(&access.folder.id)
        .bind(caller.uuid)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn restore_folder(State(state): State<AppState>, caller: Caller, Path(folder): Path<String>) -> ApiResult<Json<Value>> {
    let access = perms::folder_access(&state, &caller, &folder).await?;
    access.require_owner()?;
    if !access.in_trash {
        return Err(ApiError::conflict(
            format!("The folder \"{}\" isn't in the trash.", access.folder.name),
            "There's nothing to restore.",
        ));
    }
    // Back where it was, unless the folder above is in the trash too: then to the owner's top level. Everything in
    // it comes back with it, except what was put in the trash on its own.
    let leave_parent = sheets::folder_chain_trashed(&state, access.folder.parent_id.as_deref()).await?;
    sqlx::query(
        "update folders set trashed_at = null, trashed_by = null,
                            parent_id = case when $2 then null else parent_id end
         where id = $1",
    )
    .bind(&access.folder.id)
    .bind(leave_parent)
    .execute(&state.db)
    .await?;
    let access = perms::folder_access(&state, &caller, &folder).await?;
    Ok(Json(folder_json(&state, &access).await))
}

#[derive(Deserialize)]
struct Confirm {
    confirm: Option<String>,
}

async fn delete_forever(
    State(state): State<AppState>,
    caller: Caller,
    Path(folder): Path<String>,
    QueryParams(q): QueryParams<Confirm>,
) -> ApiResult<StatusCode> {
    let access = perms::folder_access(&state, &caller, &folder).await?;
    if access.role != Role::Owner {
        return Err(ApiError::not_allowed(
            format!("Only the owner of the folder \"{}\" can delete it forever.", access.folder.name),
            "Ask its owner.",
        ));
    }
    if !access.in_trash {
        return Err(ApiError::conflict(
            format!(
                "The folder \"{}\" isn't in the trash, and only folders in the trash can be deleted forever.",
                access.folder.name
            ),
            "Move it to the trash first, then delete it forever from there.",
        ));
    }
    if q.confirm.as_deref().map(str::trim) != Some(access.folder.name.as_str()) {
        return Err(ApiError::invalid_field(
            "confirm",
            format!(
                "Deleting the folder \"{}\" forever, with everything in it, needs its exact name as confirm.",
                access.folder.name
            ),
        )
        .with_hint("Pass ?confirm= with the folder's name. Deleting forever can't be undone."));
    }
    let mut tx = state.db.begin().await?;
    let (folder_ids, sheet_ids) = tree(&mut tx, &access.folder.id, false).await?;
    let keys = sheets::purge_sheets(&mut tx, &sheet_ids).await?;
    sqlx::query("delete from members where item_kind = 'folder' and item_id = any($1)")
        .bind(&folder_ids)
        .execute(&mut *tx)
        .await?;
    sqlx::query("delete from invites where item_kind = 'folder' and item_id = any($1)")
        .bind(&folder_ids)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "update transfers set status = 'cancelled', decided_at = now()
         where item_kind = 'folder' and item_id = any($1) and status = 'pending'",
    )
    .bind(&folder_ids)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "update folders set name = '', trashed_at = coalesce(trashed_at, now()), deleted_at = now() where id = any($1)",
    )
    .bind(&folder_ids)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    sheets::collect_blobs_later(state.clone(), keys);
    Ok(StatusCode::NO_CONTENT)
}
