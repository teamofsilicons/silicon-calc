//! Sheets: listing, creating, renaming, moving, the trash, deleting forever, `GET /v1/me`, `/v1/meta`,
//! account lookups. See docs/API.md.
//!
//! Sheets are personal: each belongs to the Carbon or Silicon who made it, except that a sheet made inside someone
//! else's shared folder belongs to that folder's owner (and still shows who made it). Moving a sheet to the trash
//! keeps everything; it can be restored at any time. Deleting it forever, from the trash and after confirming its
//! title, is the only way anything is gone: its cells, history, copies and comments are removed, and a tombstone
//! keeps its id so no other sheet ever gets it.

use std::{collections::HashMap, time::Duration};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, post},
};
use calc_engine::Workbook;
use futures::StreamExt as _;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use sqlx::PgConnection;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    accounts::Account,
    auth::{Caller, MaybeCaller},
    blobs::Removal,
    db::{self, rfc3339, rfc3339_opt},
    error::{ApiError, ApiResult, JsonBody, QueryParams},
    ids, lines,
    perms::{self, Role, SHEET_COLUMNS, SheetAccess, SheetRow},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/meta", get(meta))
        .route("/me", get(me))
        .route("/accounts/lookup", get(lookup))
        .route("/sheets", get(list).post(create))
        .route("/sheets/{sheet}", get(get_sheet).patch(patch_sheet).delete(trash_sheet))
        .route("/sheets/{sheet}/restore", post(restore_sheet))
        .route("/sheets/{sheet}/forever", delete(delete_forever))
}

/// The longest title a sheet may have.
pub const MAX_TITLE: usize = 300;
pub const DEFAULT_TITLE: &str = "Untitled sheet";

/// `Some(None)` for an explicit `null`, `None` when the field is absent.
pub(crate) fn explicit_null<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Option::deserialize(d)?))
}

/// A title as given, trimmed; an empty one is "Untitled sheet".
pub fn clean_title(title: Option<&str>) -> ApiResult<String> {
    let t = title.map(str::trim).unwrap_or("");
    if t.chars().count() > MAX_TITLE {
        return Err(ApiError::invalid_field("title", format!("A title can be at most {MAX_TITLE} characters.")));
    }
    if t.chars().any(|c| c.is_control()) {
        return Err(ApiError::invalid_field("title", "A title can't hold line breaks or other control characters."));
    }
    Ok(if t.is_empty() { DEFAULT_TITLE.to_owned() } else { t.to_owned() })
}

fn non_empty(v: Option<&str>) -> Option<&str> {
    v.map(str::trim).filter(|s| !s.is_empty())
}

// ------------------------------------------------------------------------------------------------
// Meta, me, account lookups
// ------------------------------------------------------------------------------------------------

async fn meta(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "app_id": state.cfg.app_id,
        "version": env!("CARGO_PKG_VERSION"),
        "accounts_url": state.cfg.accounts_url,
        "limits": {
            "max_rows": calc_engine::addr::MAX_ROWS,
            "max_cols": calc_engine::addr::MAX_COLS,
            "max_upload_bytes": state.cfg.max_upload_bytes,
            "max_cells_per_edit": crate::config::MAX_CELLS_PER_EDIT,
            "code_timeout_seconds": state.cfg.code_timeout.as_secs(),
        },
    }))
}

/// The caller as an `Account`, falling back to what their sign-in said when Silicon Accounts can't be asked.
pub async fn caller_account(state: &AppState, caller: &Caller) -> Account {
    let a = state.accounts.get(caller.uuid).await;
    if !a.id.is_empty() {
        return a;
    }
    let known: Option<(String, String)> = sqlx::query_as("select display_name, pfp_url from known_accounts where uuid = $1")
        .bind(caller.uuid)
        .fetch_optional(&state.db)
        .await
        .unwrap_or(None);
    let (display_name, pfp_url) = known.unwrap_or_default();
    Account {
        uuid: caller.uuid,
        id: caller.id.clone(),
        kind: caller.kind.clone(),
        display_name: if display_name.is_empty() { caller.id.clone() } else { display_name },
        pfp_url,
    }
}

async fn me(State(state): State<AppState>, caller: Caller) -> Json<Value> {
    Json(json!({"account": caller_account(&state, &caller).await}))
}

#[derive(Deserialize)]
struct LookupQuery {
    id: Option<String>,
}

async fn lookup(State(state): State<AppState>, _caller: Caller, QueryParams(q): QueryParams<LookupQuery>) -> ApiResult<Json<Account>> {
    let Some(id) = non_empty(q.id.as_deref()) else {
        return Err(ApiError::invalid_field("id", "Give the account's id, like ?id=c:ada or ?id=si:analyst."));
    };
    match state.accounts.by_public_id(id).await? {
        Some(a) => Ok(Json(a)),
        None => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("No Carbon or Silicon has the id {id}."),
            "Check the spelling; ids look like c:ada or si:analyst.",
        )),
    }
}

// ------------------------------------------------------------------------------------------------
// Sheet JSON
// ------------------------------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn render(
    state: &AppState,
    row: &SheetRow,
    role: Role,
    trashed_at: Option<OffsetDateTime>,
    version: i64,
    copies: i64,
    tabs: Option<Vec<Value>>,
    accounts: &HashMap<Uuid, Account>,
) -> Value {
    let acct = |u: Uuid| accounts.get(&u).cloned().unwrap_or_else(|| Account::unknown(u));
    let mut v = json!({
        "id": row.id,
        "title": row.title,
        "owner": acct(row.owner),
        "folder_id": row.folder_id,
        "role": role.as_str(),
        "public": {"enabled": row.public_enabled, "access": row.public_access},
        "version": version,
        "created_at": rfc3339(row.created_at),
        "created_by": acct(row.created_by),
        "updated_at": rfc3339(row.updated_at),
        "updated_by": acct(row.updated_by),
        "trashed_at": rfc3339_opt(trashed_at),
        "copies": copies,
        "url": state.cfg.sheet_url(&row.id),
    });
    if let Some(tabs) = tabs {
        v["tabs"] = Value::Array(tabs);
    }
    v
}

/// docs/API.md `Sheet` for one sheet the caller can open, with its tabs. Called by every module that answers with
/// a sheet (new sheets from imports and from old versions too). When the tabs can't be read it answers without
/// them rather than failing.
pub async fn sheet_json(state: &AppState, access: &SheetAccess) -> Value {
    match sheet_json_full(state, access).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(sheet = %access.sheet.id, error = %e.message, "reading a sheet's tabs failed; answering without them");
            let version = lines::head_version(state, &access.sheet.id).await.unwrap_or(0);
            let copies = lines::open_copies(state, &access.sheet.id).await.unwrap_or(0);
            let r = &access.sheet;
            let accounts = state.accounts.get_map([r.owner, r.created_by, r.updated_by]).await;
            render(state, r, access.role, r.trashed_at, version, copies, None, &accounts)
        }
    }
}

/// `sheet_json`, failing when anything can't be read.
pub async fn sheet_json_full(state: &AppState, access: &SheetAccess) -> ApiResult<Value> {
    let r = &access.sheet;
    let (version, tabs) = lines::sheet_tabs(state, &r.id).await?;
    let copies = lines::open_copies(state, &r.id).await?;
    let trashed_at =
        if access.in_trash { perms::trashed_since(state, r.trashed_at, r.folder_id.as_deref()).await? } else { None };
    let accounts = state.accounts.get_map([r.owner, r.created_by, r.updated_by]).await;
    Ok(render(state, r, access.role, trashed_at, version, copies, Some(tabs), &accounts))
}

/// docs/API.md `Sheet` (without tabs) for each row `me` can open, in the same order; others are left out.
pub async fn sheet_list_json(state: &AppState, me: Option<Uuid>, rows: &[SheetRow]) -> ApiResult<Vec<Value>> {
    let roles = perms::sheet_roles(state, me, rows).await?;
    let accounts = state.accounts.get_map(rows.iter().flat_map(|r| [r.owner, r.created_by, r.updated_by])).await;
    let visible: Vec<(&SheetRow, Role, bool)> =
        rows.iter().filter_map(|r| roles.get(&r.id).map(|&(role, trash)| (r, role, trash))).collect();
    // Each sheet's version, open copies and time in the trash, a few sheets at a time (owned values, so the
    // handler's future stays `Send`).
    let jobs: Vec<(AppState, String, Option<OffsetDateTime>, Option<String>, bool)> = visible
        .iter()
        .map(|&(r, _, trash)| (state.clone(), r.id.clone(), r.trashed_at, r.folder_id.clone(), trash))
        .collect();
    let extras: Vec<ApiResult<(i64, i64, Option<OffsetDateTime>)>> = futures::stream::iter(jobs)
        .map(|(state, id, own_trash, folder_id, trash)| async move {
            let version = lines::head_version(&state, &id).await?;
            let copies = lines::open_copies(&state, &id).await?;
            let trashed_at = if trash { perms::trashed_since(&state, own_trash, folder_id.as_deref()).await? } else { None };
            Ok((version, copies, trashed_at))
        })
        .buffered(8)
        .collect()
        .await;
    let mut out = Vec::with_capacity(visible.len());
    for ((r, role, _), extra) in visible.into_iter().zip(extras) {
        let (version, copies, trashed_at) = extra?;
        out.push(render(state, r, role, trashed_at, version, copies, None, &accounts));
    }
    Ok(out)
}

// ------------------------------------------------------------------------------------------------
// Listing
// ------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct ListQuery {
    scope: Option<String>,
    folder: Option<String>,
    q: Option<String>,
    cursor: Option<String>,
    limit: Option<i64>,
}

fn cursor_text(t: OffsetDateTime, id: &str) -> String {
    format!("{}.{id}", t.unix_timestamp_nanos() / 1000)
}

fn parse_cursor(c: &str) -> ApiResult<(OffsetDateTime, String)> {
    let bad = || ApiError::invalid_field("cursor", "This cursor can't be read; use the next_cursor of an earlier answer.");
    let (micros, id) = c.split_once('.').ok_or_else(bad)?;
    let micros: i128 = micros.parse().map_err(|_| bad())?;
    let t = OffsetDateTime::from_unix_timestamp_nanos(micros * 1000).map_err(|_| bad())?;
    if !ids::looks_like_id(id) {
        return Err(bad());
    }
    Ok((t, id.to_owned()))
}

/// The parameters every listing query reads, as `p.me`, `p.q`, `p.ct`, `p.cid`, `p.lim`, `p.folder`.
const PARAMS: &str = "p as (select $1::uuid as me, $2::text as q, $3::timestamptz as ct, $4::text as cid, \
                      $5::bigint as lim, $6::text as folder)";

/// Folders of the caller's in the trash, with everything under them.
const BINNED: &str = "binned(id) as (
        select f.id from folders f, p where f.owner = p.me and f.trashed_at is not null and f.deleted_at is null
        union
        select f.id from folders f join binned b on f.parent_id = b.id where f.deleted_at is null
    )";

/// Folders shared with the caller, with everything under them.
const REACH: &str = "reach(id) as (
        select f.id from members m join folders f on f.id = m.item_id, p
        where m.item_kind = 'folder' and m.account = p.me and f.deleted_at is null
        union
        select f.id from folders f join reach r on f.parent_id = r.id where f.deleted_at is null
    )";

/// Sheet candidates (`cand(id)`) without any folder above them in the trash.
fn shared_sheets_sql(cand: &str) -> String {
    format!(
        "with recursive {PARAMS}, {REACH}, cand(id) as ({cand}),
         up(sheet_id, folder_id, depth) as (
             select s.id, s.folder_id, 0 from sheets s join cand c on c.id = s.id where s.folder_id is not null
             union all
             select u.sheet_id, f.parent_id, u.depth + 1 from up u join folders f on f.id = u.folder_id
             where f.parent_id is not null and u.depth < 100
         )
         select {cols} from sheets s join cand c on c.id = s.id, p
         where not exists (select 1 from up u join folders f on f.id = u.folder_id
                           where u.sheet_id = s.id and f.trashed_at is not null)
           and (p.q is null or s.title ilike p.q)
           and (p.ct is null or (s.updated_at, s.id) < (p.ct, p.cid))
         order by s.updated_at desc, s.id desc limit (select lim from p)",
        cols = prefixed(SHEET_COLUMNS, "s")
    )
}

fn shared_folders_sql(cand: &str) -> String {
    format!(
        "with recursive {PARAMS}, {REACH}, cand(id) as ({cand}),
         up(folder_id, cur, depth) as (
             select f.id, f.parent_id, 0 from folders f join cand c on c.id = f.id where f.parent_id is not null
             union all
             select u.folder_id, f.parent_id, u.depth + 1 from up u join folders f on f.id = u.cur
             where f.parent_id is not null and u.depth < 100
         )
         select {cols} from folders f join cand c on c.id = f.id, p
         where not exists (select 1 from up u join folders a on a.id = u.cur
                           where u.folder_id = f.id and a.trashed_at is not null)
           and (p.q is null or f.name ilike p.q)
         order by lower(f.name), f.id",
        cols = prefixed(perms::FOLDER_COLUMNS, "f")
    )
}

/// `id, title` → `s.id, s.title`.
pub(crate) fn prefixed(columns: &str, alias: &str) -> String {
    columns.split(',').map(|c| format!("{alias}.{}", c.trim())).collect::<Vec<_>>().join(", ")
}

async fn list(State(state): State<AppState>, caller: Caller, QueryParams(q): QueryParams<ListQuery>) -> ApiResult<Json<Value>> {
    let scope = non_empty(q.scope.as_deref()).unwrap_or("mine").to_ascii_lowercase();
    if !matches!(scope.as_str(), "mine" | "shared" | "trash") {
        return Err(ApiError::invalid_field("scope", format!("There is no scope \"{scope}\"; use mine, shared or trash.")));
    }
    let limit = q.limit.unwrap_or(100);
    if !(1..=500).contains(&limit) {
        return Err(ApiError::invalid_field("limit", "limit must be between 1 and 500."));
    }
    let text = non_empty(q.q.as_deref()).map(db::like_anywhere);
    let cursor = non_empty(q.cursor.as_deref()).map(parse_cursor).transpose()?;
    let folder = non_empty(q.folder.as_deref()).filter(|f| *f != "root").map(str::to_owned);
    if let Some(f) = &folder {
        perms::folder_access(&state, &caller, f).await?;
    }
    let sheet_cols = prefixed(SHEET_COLUMNS, "s");
    let folder_cols = prefixed(perms::FOLDER_COLUMNS, "f");
    let by_trash_time = scope == "trash" && folder.is_none();
    let (sheets_sql, folders_sql): (String, String) = match (folder.is_some(), scope.as_str(), text.is_some()) {
        (true, _, _) => (
            format!(
                "with {PARAMS} select {sheet_cols} from sheets s, p
                 where s.folder_id = p.folder and s.deleted_at is null and s.trashed_at is null
                   and (p.q is null or s.title ilike p.q)
                   and (p.ct is null or (s.updated_at, s.id) < (p.ct, p.cid))
                 order by s.updated_at desc, s.id desc limit (select lim from p)"
            ),
            format!(
                "with {PARAMS} select {folder_cols} from folders f, p
                 where f.parent_id = p.folder and f.deleted_at is null and f.trashed_at is null
                   and (p.q is null or f.name ilike p.q)
                 order by lower(f.name), f.id"
            ),
        ),
        (false, "mine", false) => (
            format!(
                "with {PARAMS} select {sheet_cols} from sheets s, p
                 where s.owner = p.me and s.folder_id is null and s.deleted_at is null and s.trashed_at is null
                   and (p.ct is null or (s.updated_at, s.id) < (p.ct, p.cid))
                 order by s.updated_at desc, s.id desc limit (select lim from p)"
            ),
            format!(
                "with {PARAMS} select {folder_cols} from folders f, p
                 where f.owner = p.me and f.parent_id is null and f.deleted_at is null and f.trashed_at is null
                 order by lower(f.name), f.id"
            ),
        ),
        (false, "mine", true) => (
            format!(
                "with recursive {PARAMS}, {BINNED} select {sheet_cols} from sheets s, p
                 where s.owner = p.me and s.deleted_at is null and s.trashed_at is null
                   and (s.folder_id is null or s.folder_id not in (select id from binned))
                   and s.title ilike p.q
                   and (p.ct is null or (s.updated_at, s.id) < (p.ct, p.cid))
                 order by s.updated_at desc, s.id desc limit (select lim from p)"
            ),
            format!(
                "with recursive {PARAMS}, {BINNED} select {folder_cols} from folders f, p
                 where f.owner = p.me and f.deleted_at is null and f.id not in (select id from binned)
                   and f.name ilike p.q
                 order by lower(f.name), f.id"
            ),
        ),
        (false, "shared", searching) => {
            let direct_sheets = "select m.item_id from members m join sheets s on s.id = m.item_id, p
                 where m.item_kind = 'sheet' and m.account = p.me and s.owner <> p.me
                   and s.deleted_at is null and s.trashed_at is null";
            let direct_folders = "select m.item_id from members m join folders f on f.id = m.item_id, p
                 where m.item_kind = 'folder' and m.account = p.me and f.owner <> p.me
                   and f.deleted_at is null and f.trashed_at is null";
            if searching {
                // Searching looks inside shared folders too.
                (
                    shared_sheets_sql(&format!(
                        "{direct_sheets} union select s.id from reach r join sheets s on s.folder_id = r.id, p
                         where s.owner <> p.me and s.deleted_at is null and s.trashed_at is null"
                    )),
                    shared_folders_sql(&format!(
                        "{direct_folders} union select f.id from reach r join folders f on f.id = r.id, p
                         where f.owner <> p.me and f.trashed_at is null"
                    )),
                )
            } else {
                (shared_sheets_sql(direct_sheets), shared_folders_sql(direct_folders))
            }
        }
        _ => (
            format!(
                "with {PARAMS} select {sheet_cols} from sheets s, p
                 where s.owner = p.me and s.trashed_at is not null and s.deleted_at is null
                   and (p.q is null or s.title ilike p.q)
                   and (p.ct is null or (s.trashed_at, s.id) < (p.ct, p.cid))
                 order by s.trashed_at desc, s.id desc limit (select lim from p)"
            ),
            format!(
                "with {PARAMS} select {folder_cols} from folders f, p
                 where f.owner = p.me and f.trashed_at is not null and f.deleted_at is null
                   and (p.q is null or f.name ilike p.q)
                 order by f.trashed_at desc, f.id"
            ),
        ),
    };
    let (ct, cid) = match &cursor {
        Some((t, id)) => (Some(*t), Some(id.clone())),
        None => (None, None),
    };
    let mut rows: Vec<SheetRow> = sqlx::query_as(sqlx::AssertSqlSafe(sheets_sql))
        .bind(caller.uuid)
        .bind(&text)
        .bind(ct)
        .bind(&cid)
        .bind(limit + 1)
        .bind(&folder)
        .fetch_all(&state.db)
        .await?;
    let more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let next_cursor = if more {
        rows.last().map(|r| cursor_text(if by_trash_time { r.trashed_at.unwrap_or(r.updated_at) } else { r.updated_at }, &r.id))
    } else {
        None
    };
    let folders = if cursor.is_none() {
        let frows: Vec<perms::FolderRow> = sqlx::query_as(sqlx::AssertSqlSafe(folders_sql))
            .bind(caller.uuid)
            .bind(&text)
            .bind(ct)
            .bind(&cid)
            .bind(limit + 1)
            .bind(&folder)
            .fetch_all(&state.db)
            .await?;
        crate::folders::folder_list_json(&state, caller.uuid, &frows).await?
    } else {
        Vec::new()
    };
    let sheets = sheet_list_json(&state, Some(caller.uuid), &rows).await?;
    Ok(Json(json!({"sheets": sheets, "folders": folders, "next_cursor": next_cursor})))
}

// ------------------------------------------------------------------------------------------------
// Creating
// ------------------------------------------------------------------------------------------------

/// Where something new goes and who owns it: in a folder, that folder's owner (the caller must be able to edit
/// the folder); otherwise the caller, at their top level.
pub async fn placement(state: &AppState, caller: &Caller, folder_id: Option<&str>) -> ApiResult<(Option<String>, Uuid)> {
    match non_empty(folder_id).filter(|f| *f != "root") {
        Some(f) => {
            let access = perms::folder_access(state, caller, f).await.map_err(|e| {
                if e.code == "not_found" {
                    ApiError::invalid_field("folder_id", format!("There is no folder {f} that you can open."))
                } else {
                    e
                }
            })?;
            if !access.role.can_edit() {
                return Err(ApiError::not_allowed(
                    format!(
                        "You can't add to the folder \"{}\": you are {} on it.",
                        access.folder.name,
                        access.role.with_article()
                    ),
                    "Ask its owner to share it with you as an editor.",
                ));
            }
            access.require_live()?;
            Ok((Some(access.folder.id), access.folder.owner))
        }
        None => Ok((None, caller.uuid)),
    }
}

/// Creates a sheet holding `wb` (its main line at version 0), placed and owned as `placement` says, and answers the
/// caller's access to it. For every way a sheet is made: new, imported from a file, made from an old version.
pub async fn create_sheet(
    state: &AppState,
    caller: &Caller,
    title: Option<&str>,
    folder_id: Option<&str>,
    wb: &Workbook,
) -> ApiResult<SheetAccess> {
    let title = clean_title(title)?;
    let (folder, owner) = placement(state, caller, folder_id).await?;
    let search_text = crate::search::search_text_of(wb);
    for _ in 0..8 {
        let id = ids::sheet();
        let mut tx = state.db.begin().await?;
        // A taken id (a live sheet or the tombstone of one deleted forever) is never reused.
        let inserted = sqlx::query(
            "insert into sheets (id, title, folder_id, owner, created_by, updated_by, search_text)
             values ($1, $2, $3, $4, $5, $5, $6) on conflict (id) do nothing",
        )
        .bind(&id)
        .bind(&title)
        .bind(&folder)
        .bind(owner)
        .bind(caller.uuid)
        .bind(&search_text)
        .execute(&mut *tx)
        .await?;
        if inserted.rows_affected() == 0 {
            continue;
        }
        lines::create_main_line(state, &mut tx, &id, caller.uuid, wb).await?;
        tx.commit().await?;
        return perms::sheet_access(state, Some(caller), &id).await;
    }
    Err(ApiError::internal("eight new sheet ids in a row were taken"))
}

#[derive(Deserialize)]
struct NewSheet {
    title: Option<String>,
    folder_id: Option<String>,
}

async fn create(State(state): State<AppState>, caller: Caller, JsonBody(body): JsonBody<NewSheet>) -> ApiResult<(StatusCode, Json<Value>)> {
    let access = create_sheet(&state, &caller, body.title.as_deref(), body.folder_id.as_deref(), &Workbook::new()).await?;
    Ok((StatusCode::CREATED, Json(sheet_json_full(&state, &access).await?)))
}

// ------------------------------------------------------------------------------------------------
// Reading and changing
// ------------------------------------------------------------------------------------------------

async fn get_sheet(State(state): State<AppState>, MaybeCaller(caller): MaybeCaller, Path(sheet): Path<String>) -> ApiResult<Json<Value>> {
    let access = perms::sheet_access(&state, caller.as_ref(), &sheet).await?;
    Ok(Json(sheet_json_full(&state, &access).await?))
}

#[derive(Deserialize)]
struct SheetPatch {
    title: Option<String>,
    /// `null` moves the sheet to its owner's top level.
    #[serde(default, deserialize_with = "explicit_null")]
    folder_id: Option<Option<String>>,
}

/// Whether the caller may move the sheet to `target` (a folder, or the owner's top level).
async fn check_move(state: &AppState, caller: &Caller, access: &SheetAccess, target: Option<&str>) -> ApiResult<()> {
    let sheet = &access.sheet;
    if target == sheet.folder_id.as_deref() {
        return Ok(());
    }
    match target {
        Some(f) => {
            let into = perms::folder_access(state, caller, f).await.map_err(|e| {
                if e.code == "not_found" {
                    ApiError::invalid_field("folder_id", format!("There is no folder {f} that you can open."))
                } else {
                    e
                }
            })?;
            into.require_edit()?;
            if into.folder.owner != sheet.owner {
                let accounts = state.accounts.get_map([sheet.owner, into.folder.owner]).await;
                let show = |u: &Uuid| accounts.get(u).map(|a| a.id.clone()).unwrap_or_default();
                return Err(ApiError::conflict(
                    format!(
                        "\"{}\" belongs to {} and the folder \"{}\" belongs to {}, and a folder only holds what its owner owns.",
                        sheet.title,
                        show(&sheet.owner),
                        into.folder.name,
                        show(&into.folder.owner)
                    ),
                    format!("Transfer the sheet to {} first (calc transfer), then move it.", show(&into.folder.owner)),
                ));
            }
        }
        None => {
            if access.role != Role::Owner {
                return Err(ApiError::not_allowed(
                    format!("Only the owner of \"{}\" can move it out of its folders.", sheet.title),
                    "Move it into another folder you can edit, or ask its owner.",
                ));
            }
        }
    }
    if let Some(from) = &sheet.folder_id {
        // Taking it out of a folder changes that folder: it needs editing there too.
        perms::folder_access(state, caller, from).await?.require_edit()?;
    }
    Ok(())
}

async fn patch_sheet(
    State(state): State<AppState>,
    caller: Caller,
    Path(sheet): Path<String>,
    JsonBody(body): JsonBody<SheetPatch>,
) -> ApiResult<Json<Value>> {
    let access = perms::sheet_access(&state, Some(&caller), &sheet).await?;
    if body.title.is_none() && body.folder_id.is_none() {
        return Ok(Json(sheet_json_full(&state, &access).await?));
    }
    access.require_edit()?;
    let title = match &body.title {
        Some(t) if t.trim().is_empty() => return Err(ApiError::invalid_field("title", "A sheet's title can't be empty.")),
        Some(t) => Some(clean_title(Some(t))?),
        None => None,
    };
    let target: Option<Option<String>> =
        body.folder_id.map(|f| non_empty(f.as_deref()).filter(|f| *f != "root").map(str::to_owned));
    if let Some(t) = &target {
        check_move(&state, &caller, &access, t.as_deref()).await?;
    }
    sqlx::query(
        "update sheets set title = coalesce($2, title),
                           folder_id = case when $3 then $4 else folder_id end,
                           updated_at = now(), updated_by = $5
         where id = $1",
    )
    .bind(&access.sheet.id)
    .bind(&title)
    .bind(target.is_some())
    .bind(target.flatten())
    .bind(caller.uuid)
    .execute(&state.db)
    .await?;
    let access = perms::sheet_access(&state, Some(&caller), &sheet).await?;
    Ok(Json(sheet_json_full(&state, &access).await?))
}

// ------------------------------------------------------------------------------------------------
// The trash
// ------------------------------------------------------------------------------------------------

async fn trash_sheet(State(state): State<AppState>, caller: Caller, Path(sheet): Path<String>) -> ApiResult<StatusCode> {
    let access = perms::sheet_access(&state, Some(&caller), &sheet).await?;
    if access.role != Role::Owner {
        return Err(ApiError::not_allowed(
            format!("Only the owner of \"{}\" can move it to the trash.", access.sheet.title),
            "Ask its owner, or leave the sheet instead (calc share leave).",
        ));
    }
    sqlx::query("update sheets set trashed_at = now(), trashed_by = $2 where id = $1 and trashed_at is null")
        .bind(&access.sheet.id)
        .bind(caller.uuid)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Whether any folder above is in the trash.
pub(crate) async fn folder_chain_trashed(state: &AppState, folder_id: Option<&str>) -> ApiResult<bool> {
    Ok(perms::ancestors(state, folder_id).await?.iter().any(|f| f.trashed_at.is_some()))
}

async fn restore_sheet(State(state): State<AppState>, caller: Caller, Path(sheet): Path<String>) -> ApiResult<Json<Value>> {
    let access = perms::sheet_access(&state, Some(&caller), &sheet).await?;
    access.require_owner()?;
    if !access.in_trash {
        return Err(ApiError::conflict(
            format!("\"{}\" isn't in the trash.", access.sheet.title),
            "There's nothing to restore. To bring back an older version, restore that version instead.",
        ));
    }
    // Back where it was, unless that folder is in the trash too: then to the owner's top level.
    let leave_folder = folder_chain_trashed(&state, access.sheet.folder_id.as_deref()).await?;
    sqlx::query(
        "update sheets set trashed_at = null, trashed_by = null,
                           folder_id = case when $2 then null else folder_id end
         where id = $1",
    )
    .bind(&access.sheet.id)
    .bind(leave_folder)
    .execute(&state.db)
    .await?;
    let access = perms::sheet_access(&state, Some(&caller), &sheet).await?;
    Ok(Json(sheet_json_full(&state, &access).await?))
}

#[derive(Deserialize)]
struct Confirm {
    confirm: Option<String>,
}

async fn delete_forever(
    State(state): State<AppState>,
    caller: Caller,
    Path(sheet): Path<String>,
    QueryParams(q): QueryParams<Confirm>,
) -> ApiResult<StatusCode> {
    let access = perms::sheet_access(&state, Some(&caller), &sheet).await?;
    if access.role != Role::Owner {
        return Err(ApiError::not_allowed(
            format!("Only the owner of \"{}\" can delete it forever.", access.sheet.title),
            "Ask its owner.",
        ));
    }
    if !access.in_trash {
        return Err(ApiError::conflict(
            format!("\"{}\" isn't in the trash, and only sheets in the trash can be deleted forever.", access.sheet.title),
            "Move it to the trash first (calc trash), then delete it forever from there.",
        ));
    }
    if q.confirm.as_deref().map(str::trim) != Some(access.sheet.title.as_str()) {
        return Err(ApiError::invalid_field(
            "confirm",
            format!("Deleting \"{}\" forever needs its exact title as confirm.", access.sheet.title),
        )
        .with_hint("Pass ?confirm= with the sheet's title. Deleting forever can't be undone."));
    }
    let mut tx = state.db.begin().await?;
    let locked: Option<(String,)> = sqlx::query_as("select id from sheets where id = $1 and deleted_at is null for update")
        .bind(&access.sheet.id)
        .fetch_optional(&mut *tx)
        .await?;
    if locked.is_none() {
        return Err(ApiError::not_found("This sheet"));
    }
    let keys = purge_sheets(&mut tx, std::slice::from_ref(&access.sheet.id)).await?;
    tx.commit().await?;
    collect_blobs_later(state.clone(), keys);
    Ok(StatusCode::NO_CONTENT)
}

/// Removes sheets for good, inside the caller's transaction: everything that points at them (lines, history,
/// snapshots, versions, comments, and whatever other modules keep), their members and invites; pending transfers
/// of them are cancelled. Each keeps a tombstone row (`deleted_at`, no title) so its id is never reused. Answers the
/// blob keys their history used, to remove once nothing points at them (`collect_blobs_later`).
pub(crate) async fn purge_sheets(conn: &mut PgConnection, sheet_ids: &[String]) -> ApiResult<Vec<String>> {
    if sheet_ids.is_empty() {
        return Ok(Vec::new());
    }
    let keys: Vec<(String,)> = sqlx::query_as(
        "with l as (select id from lines where sheet_id = any($1))
         select blob from snapshots where line_id in (select id from l)
         union select op_blob from ops where op_blob is not null and line_id in (select id from l)
         union select inverse_blob from ops where inverse_blob is not null and line_id in (select id from l)",
    )
    .bind(sheet_ids)
    .fetch_all(&mut *conn)
    .await?;
    db::purge_references(conn, "sheets", "id", "select id from sheets where id = any($1)", sheet_ids).await?;
    sqlx::query("delete from members where item_kind = 'sheet' and item_id = any($1)")
        .bind(sheet_ids)
        .execute(&mut *conn)
        .await?;
    sqlx::query("delete from invites where item_kind = 'sheet' and item_id = any($1)")
        .bind(sheet_ids)
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "update transfers set status = 'cancelled', decided_at = now()
         where item_kind = 'sheet' and item_id = any($1) and status = 'pending'",
    )
    .bind(sheet_ids)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "update sheets set title = '', search_text = '', folder_id = null, public_enabled = false,
                           trashed_at = coalesce(trashed_at, now()), deleted_at = now()
         where id = any($1)",
    )
    .bind(sheet_ids)
    .execute(&mut *conn)
    .await?;
    Ok(keys.into_iter().map(|k| k.0).collect())
}

/// How recently a blob must not have been used for it to be removed.
pub const BLOB_GRACE: Duration = Duration::from_secs(600);

/// Removes the blobs among `keys` that nothing points at any more, now (those used within `grace` stay), and
/// answers what happened to each.
pub async fn collect_blobs(state: &AppState, keys: &[String], grace: Duration) -> Vec<(String, Removal)> {
    let mut out = Vec::new();
    for key in keys {
        let in_use = || async {
            let used = db::blob_keys_in_use(&state.db, std::slice::from_ref(key)).await?;
            Ok(used.contains(key))
        };
        match state.blobs.remove_unused(key, grace, in_use).await {
            Ok(r) => out.push((key.clone(), r)),
            Err(e) => tracing::warn!(key = %key, error = %e.message, "removing a blob failed"),
        }
    }
    out
}

/// `collect_blobs` in the background, trying recently used blobs again after the grace period.
pub(crate) fn collect_blobs_later(state: AppState, keys: Vec<String>) {
    if keys.is_empty() {
        return;
    }
    tokio::spawn(async move {
        let mut keys = keys;
        for _ in 0..3 {
            let recent: Vec<String> = collect_blobs(&state, &keys, BLOB_GRACE)
                .await
                .into_iter()
                .filter(|(_, r)| *r == Removal::Recent)
                .map(|(k, _)| k)
                .collect();
            if recent.is_empty() {
                return;
            }
            keys = recent;
            tokio::time::sleep(BLOB_GRACE + Duration::from_secs(5)).await;
        }
    });
}
