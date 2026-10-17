//! Sharing sheets and folders with Carbons and Silicons, by c:id, si:id or email, and public links.
//! See docs/API.md, Sharing.
//!
//! The owner and editors invite (as editor, commenter or viewer, never above their own role), change roles and set
//! the public link; anyone can leave. Sharing a folder shares everything in it, including what is added later, and
//! the higher of direct and folder access wins. An email with no account yet waits as an invite and becomes a
//! membership when an account with that verified email first signs in to Calc. Everyone who gains access hears
//! about it through Silicon Ting (`calc.sheet.shared`), written in the same transaction as the membership.

use std::collections::HashMap;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, patch, put},
};
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    accounts::Account,
    auth::Caller,
    db::rfc3339,
    error::{ApiError, ApiResult, JsonBody},
    folders, notify,
    perms::{self, Role},
    sheets,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/sheets/{sheet}/sharing", get(sheet_sharing).post(sheet_invite))
        .route("/sheets/{sheet}/sharing/{account}", patch(sheet_change).delete(sheet_remove))
        .route("/sheets/{sheet}/invites/{email}", delete(sheet_uninvite))
        .route("/sheets/{sheet}/public", put(sheet_public))
        .route("/folders/{folder}/sharing", get(folder_sharing).post(folder_invite))
        .route("/folders/{folder}/sharing/{account}", patch(folder_change).delete(folder_remove))
        .route("/folders/{folder}/invites/{email}", delete(folder_uninvite))
        .route("/folders/{folder}/public", put(folder_public))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Sheet,
    Folder,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sheet => "sheet",
            Self::Folder => "folder",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "sheet" => Some(Self::Sheet),
            "folder" => Some(Self::Folder),
            _ => None,
        }
    }
}

/// A sheet or folder, as sharing needs it.
struct Item {
    kind: Kind,
    id: String,
    title: String,
    owner: Uuid,
    /// The folder it sits in (where folder access comes from).
    parent: Option<String>,
    role: Role,
    in_trash: bool,
    public: Option<(bool, String)>,
}

impl Item {
    fn what(&self) -> String {
        match self.kind {
            Kind::Sheet => format!("\"{}\"", self.title),
            Kind::Folder => format!("the folder \"{}\"", self.title),
        }
    }

    fn require_share(&self) -> ApiResult<()> {
        if !self.role.can_share() {
            return Err(ApiError::not_allowed(
                format!(
                    "You can't share {}: you are {} on it, and only its owner and editors can.",
                    self.what(),
                    self.role.with_article()
                ),
                "Ask its owner or an editor to invite them.",
            ));
        }
        if self.in_trash {
            return Err(ApiError::conflict(
                format!("{} is in the trash, so its sharing can't change.", capitalize(&self.what())),
                "Restore it from the trash first (calc restore).",
            ));
        }
        Ok(())
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) if f.is_lowercase() => f.to_uppercase().collect::<String>() + c.as_str(),
        _ => s.to_owned(),
    }
}

async fn item(state: &AppState, caller: &Caller, kind: Kind, id: &str) -> ApiResult<Item> {
    match kind {
        Kind::Sheet => {
            let a = perms::sheet_access(state, Some(caller), id).await?;
            Ok(Item {
                kind,
                id: a.sheet.id.clone(),
                title: a.sheet.title.clone(),
                owner: a.sheet.owner,
                parent: a.sheet.folder_id.clone(),
                role: a.role,
                in_trash: a.in_trash,
                public: Some((a.sheet.public_enabled, a.sheet.public_access.clone())),
            })
        }
        Kind::Folder => {
            let a = perms::folder_access(state, caller, id).await?;
            Ok(Item {
                kind,
                id: a.folder.id.clone(),
                title: a.folder.name.clone(),
                owner: a.folder.owner,
                parent: a.folder.parent_id.clone(),
                role: a.role,
                in_trash: a.in_trash,
                public: None,
            })
        }
    }
}

/// An account's role on an item through ownership or membership (direct or through a folder), ignoring the
/// public link; None when they have none.
async fn member_role(state: &AppState, kind: Kind, item_id: &str, account: Uuid) -> ApiResult<Option<Role>> {
    match kind {
        Kind::Sheet => {
            let Some(sheet) = perms::load_sheet(state, item_id).await? else { return Ok(None) };
            let mut private = sheet.clone();
            private.public_enabled = false;
            Ok(perms::sheet_roles(state, Some(account), std::slice::from_ref(&private)).await?.remove(&sheet.id).map(|r| r.0))
        }
        Kind::Folder => perms::folder_role_of(state, account, item_id).await,
    }
}

// ------------------------------------------------------------------------------------------------
// The sharing answer
// ------------------------------------------------------------------------------------------------

async fn sharing_json(state: &AppState, caller: &Caller, item: &Item) -> ApiResult<Value> {
    let direct: Vec<(Uuid, String, Uuid, OffsetDateTime)> = sqlx::query_as(
        "select account, role, invited_by, created_at from members where item_kind = $1 and item_id = $2 order by created_at",
    )
    .bind(item.kind.as_str())
    .bind(&item.id)
    .fetch_all(&state.db)
    .await?;
    let chain = perms::ancestors(state, item.parent.as_deref()).await?;
    let chain_ids: Vec<String> = chain.iter().map(|f| f.id.clone()).collect();
    let inherited: Vec<(String, Uuid, String, Uuid, OffsetDateTime)> = sqlx::query_as(
        "select item_id, account, role, invited_by, created_at from members
         where item_kind = 'folder' and item_id = any($1) order by created_at",
    )
    .bind(&chain_ids)
    .fetch_all(&state.db)
    .await?;
    let invites: Vec<(String, String, Uuid, OffsetDateTime)> = if item.role.can_share() {
        sqlx::query_as(
            "select email::text, role, invited_by, created_at from invites where item_kind = $1 and item_id = $2
             order by created_at",
        )
        .bind(item.kind.as_str())
        .bind(&item.id)
        .fetch_all(&state.db)
        .await?
    } else {
        Vec::new()
    };
    let mut uuids = vec![item.owner];
    uuids.extend(direct.iter().flat_map(|d| [d.0, d.2]));
    uuids.extend(inherited.iter().flat_map(|i| [i.1, i.3]));
    uuids.extend(invites.iter().map(|i| i.2));
    let accounts = state.accounts.get_map(uuids).await;
    let acct = |u: Uuid| accounts.get(&u).cloned().unwrap_or_else(|| Account::unknown(u));

    // One entry per account: the role that wins, and where it comes from (direct on a tie, else the nearest folder).
    struct Entry {
        role: Role,
        via: Option<String>,
        direct: Option<Role>,
        invited_by: Uuid,
        since: OffsetDateTime,
    }
    let mut order: Vec<Uuid> = Vec::new();
    let mut entries: HashMap<Uuid, Entry> = HashMap::new();
    for (account, role, invited_by, since) in &direct {
        let Some(role) = Role::parse(role) else { continue };
        if *account == item.owner {
            continue;
        }
        order.push(*account);
        entries.insert(*account, Entry { role, via: None, direct: Some(role), invited_by: *invited_by, since: *since });
    }
    // Nearest folder first, so a tie keeps the nearest.
    for folder in &chain {
        for (fid, account, role, invited_by, since) in inherited.iter().filter(|i| i.0 == folder.id) {
            let Some(role) = Role::parse(role) else { continue };
            if *account == item.owner {
                continue;
            }
            match entries.get_mut(account) {
                Some(e) if role > e.role => {
                    e.role = role;
                    e.via = Some(fid.clone());
                    e.invited_by = *invited_by;
                    e.since = *since;
                }
                Some(_) => {}
                None => {
                    order.push(*account);
                    entries.insert(
                        *account,
                        Entry { role, via: Some(fid.clone()), direct: None, invited_by: *invited_by, since: *since },
                    );
                }
            }
        }
    }
    let mut folder_refs: HashMap<String, Value> = HashMap::new();
    for f in &chain {
        if entries.values().any(|e| e.via.as_deref() == Some(f.id.as_str())) {
            folder_refs.insert(f.id.clone(), folders::folder_ref_json(state, Some(caller.uuid), f).await?);
        }
    }
    let members: Vec<Value> = order
        .iter()
        .filter_map(|u| entries.get(u).map(|e| (u, e)))
        .map(|(u, e)| {
            json!({
                "account": acct(*u),
                "role": e.role.as_str(),
                "via": match &e.via {
                    None => json!("direct"),
                    Some(f) => json!({"folder": folder_refs.get(f).cloned().unwrap_or(Value::Null)}),
                },
                "direct_role": e.direct.map(Role::as_str),
                "invited_by": acct(e.invited_by),
                "since": rfc3339(e.since),
            })
        })
        .collect();
    let invites: Vec<Value> = invites
        .iter()
        .map(|(email, role, by, at)| json!({"email": email, "role": role, "invited_by": acct(*by), "created_at": rfc3339(*at)}))
        .collect();
    let public = match &item.public {
        Some((enabled, access)) => json!({"enabled": enabled, "access": access}),
        None => json!({"enabled": false, "access": "view"}),
    };
    Ok(json!({
        "owner": acct(item.owner),
        "role": item.role.as_str(),
        "members": members,
        "invites": invites,
        "public": public,
    }))
}

async fn show(state: AppState, caller: Caller, kind: Kind, id: String) -> ApiResult<Json<Value>> {
    let item = item(&state, &caller, kind, &id).await?;
    Ok(Json(sharing_json(&state, &caller, &item).await?))
}

async fn sheet_sharing(State(state): State<AppState>, caller: Caller, Path(sheet): Path<String>) -> ApiResult<Json<Value>> {
    show(state, caller, Kind::Sheet, sheet).await
}

async fn folder_sharing(State(state): State<AppState>, caller: Caller, Path(folder): Path<String>) -> ApiResult<Json<Value>> {
    show(state, caller, Kind::Folder, folder).await
}

// ------------------------------------------------------------------------------------------------
// Inviting
// ------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
struct InviteBody {
    invite: Option<OneOrMany>,
    role: Option<String>,
    notify: Option<bool>,
}

fn looks_like_email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else { return false };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !s.contains(char::is_whitespace)
        && !domain.contains('@')
}

fn shareable(role: Option<&str>, field: &str) -> ApiResult<Role> {
    let r = role.unwrap_or("viewer");
    Role::parse_shareable(r).ok_or_else(|| {
        ApiError::invalid_field(field, format!("\"{r}\" isn't a role here; use editor, commenter or viewer."))
    })
}

fn not_above(item: &Item, role: Role) -> ApiResult<()> {
    if role > item.role {
        return Err(ApiError::not_allowed(
            format!(
                "You can't make anyone {} of {}: you are {} on it, and no one can give more than they have.",
                role.with_article(),
                item.what(),
                item.role.with_article()
            ),
            "Pick a role no higher than your own, or ask its owner.",
        ));
    }
    Ok(())
}

/// The ting data for `calc.sheet.shared`.
fn shared_data(state: &AppState, kind: Kind, id: &str, title: &str, role: Role, by: &Account) -> Value {
    let (url, cli) = match kind {
        Kind::Sheet => (state.cfg.sheet_url(id), format!("calc sheets show {id}")),
        Kind::Folder => (state.cfg.folder_url(id), format!("calc folders show {id}")),
    };
    let item = match kind {
        Kind::Sheet => json!({"sheet": {"id": id, "title": title}}),
        Kind::Folder => json!({"folder": {"id": id, "name": title}}),
    };
    let mut data = json!({"kind": kind.as_str(), "role": role.as_str(), "by": by, "url": url, "cli": cli});
    if let (Some(d), Some(i)) = (data.as_object_mut(), item.as_object()) {
        d.extend(i.clone());
    }
    data
}

async fn queue_shared(
    conn: &mut sqlx::PgConnection,
    state: &AppState,
    kind: Kind,
    id: &str,
    title: &str,
    role: Role,
    to: Uuid,
    by: &Account,
) -> ApiResult<()> {
    let key = format!("calc.sheet.shared:{}:{id}:{to}:{}", kind.as_str(), Uuid::now_v7());
    notify::enqueue(conn, "calc.sheet.shared", to, &key, shared_data(state, kind, id, title, role, by)).await
}

async fn invite(state: AppState, caller: Caller, kind: Kind, id: String, body: InviteBody) -> ApiResult<Json<Value>> {
    let item = item(&state, &caller, kind, &id).await?;
    item.require_share()?;
    let role = shareable(body.role.as_deref(), "role")?;
    not_above(&item, role)?;
    let notify_them = body.notify.unwrap_or(true);
    let wanted: Vec<String> = match body.invite {
        Some(OneOrMany::One(s)) => vec![s],
        Some(OneOrMany::Many(v)) => v,
        None => Vec::new(),
    };
    let wanted: Vec<String> = wanted.into_iter().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect();
    if wanted.is_empty() {
        return Err(ApiError::invalid_field("invite", "Name at least one c:id, si:id or email address to invite."));
    }
    if wanted.len() > 100 {
        return Err(ApiError::invalid_field("invite", "Invite at most 100 at a time."));
    }
    // Everyone is checked before anything changes.
    let mut accounts: Vec<Account> = Vec::new();
    let mut emails: Vec<String> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    for w in &wanted {
        let lower = w.to_ascii_lowercase();
        if lower.starts_with("c:") || lower.starts_with("si:") {
            match state.accounts.by_public_id(&lower).await {
                Ok(Some(a)) => accounts.push(a),
                Ok(None) => unknown.push(w.clone()),
                Err(e) if e.code == "invalid_field" => unknown.push(w.clone()),
                Err(e) => return Err(e),
            }
        } else if looks_like_email(&lower) {
            // An address an account already signed in to Calc with is that account.
            let known: Option<(Uuid,)> = sqlx::query_as(
                "select uuid from known_accounts where email = $1::citext order by last_seen_at desc limit 1",
            )
            .bind(&lower)
            .fetch_optional(&state.db)
            .await?;
            match known {
                Some((u,)) => accounts.push(state.accounts.get(u).await),
                None => emails.push(lower),
            }
        } else {
            unknown.push(w.clone());
        }
    }
    if !unknown.is_empty() {
        let list = unknown.join(", ");
        let message = if unknown.len() == 1 {
            format!("No Carbon or Silicon has the id {list}, and it isn't an email address.")
        } else {
            format!("No Carbon or Silicon has these ids, and they aren't email addresses: {list}.")
        };
        return Err(ApiError::invalid_field("invite", message.clone())
            .with_hint("Check the ids (calc accounts lookup c:…), or invite them by email instead.")
            .with_details(json!({"fields": {"invite": message}, "unknown": unknown})));
    }
    let inviter = sheets::caller_account(&state, &caller).await;
    // Who already had access (so only those who gain it are told).
    let mut had_access: HashMap<Uuid, bool> = HashMap::new();
    for a in &accounts {
        if !had_access.contains_key(&a.uuid) {
            had_access.insert(a.uuid, member_role(&state, kind, &item.id, a.uuid).await?.is_some());
        }
    }
    let mut tx = state.db.begin().await?;
    let mut done: Vec<Uuid> = Vec::new();
    for a in &accounts {
        if a.uuid == item.owner || done.contains(&a.uuid) {
            continue;
        }
        done.push(a.uuid);
        let res = sqlx::query(
            "insert into members (item_kind, item_id, account, role, invited_by) values ($1, $2, $3, $4, $5)
             on conflict (item_kind, item_id, account) do update set role = excluded.role",
        )
        .bind(kind.as_str())
        .bind(&item.id)
        .bind(a.uuid)
        .bind(role.as_str())
        .bind(caller.uuid)
        .execute(&mut *tx)
        .await?;
        if res.rows_affected() > 0 && notify_them && !had_access.get(&a.uuid).copied().unwrap_or(false) && a.uuid != caller.uuid {
            queue_shared(&mut tx, &state, kind, &item.id, &item.title, role, a.uuid, &inviter).await?;
        }
    }
    for email in &emails {
        sqlx::query(
            "insert into invites (item_kind, item_id, email, role, invited_by, notify) values ($1, $2, $3, $4, $5, $6)
             on conflict (item_kind, item_id, email) do update
               set role = excluded.role, invited_by = excluded.invited_by, notify = excluded.notify",
        )
        .bind(kind.as_str())
        .bind(&item.id)
        .bind(email)
        .bind(role.as_str())
        .bind(caller.uuid)
        .bind(notify_them)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    state.notify.wake();
    let item = self::item(&state, &caller, kind, &id).await?;
    Ok(Json(sharing_json(&state, &caller, &item).await?))
}

async fn sheet_invite(State(state): State<AppState>, caller: Caller, Path(sheet): Path<String>, JsonBody(body): JsonBody<InviteBody>) -> ApiResult<Json<Value>> {
    invite(state, caller, Kind::Sheet, sheet, body).await
}

async fn folder_invite(State(state): State<AppState>, caller: Caller, Path(folder): Path<String>, JsonBody(body): JsonBody<InviteBody>) -> ApiResult<Json<Value>> {
    invite(state, caller, Kind::Folder, folder, body).await
}

/// Turns every invite to `email` into a membership for `account` (called when an account with that verified email
/// signs in to Calc), telling each new member through Ting. Answers how many became memberships.
pub async fn accept_invites(state: &AppState, account: Uuid, email: &str) -> ApiResult<usize> {
    let mut tx = state.db.begin().await?;
    let taken: Vec<(String, String, String, Uuid, bool)> = sqlx::query_as(
        "delete from invites where email = $1::citext returning item_kind, item_id, role, invited_by, notify",
    )
    .bind(email)
    .fetch_all(&mut *tx)
    .await?;
    if taken.is_empty() {
        return Ok(0);
    }
    let mut made = 0;
    let mut tings: Vec<(Kind, String, String, Role, Uuid)> = Vec::new();
    for (kind, item_id, role, invited_by, notify_them) in taken {
        let (Some(kind), Some(role)) = (Kind::parse(&kind), Role::parse_shareable(&role)) else { continue };
        let row: Option<(Uuid, String)> = match kind {
            Kind::Sheet => sqlx::query_as("select owner, title from sheets where id = $1 and deleted_at is null"),
            Kind::Folder => sqlx::query_as("select owner, name from folders where id = $1 and deleted_at is null"),
        }
        .bind(&item_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((owner, title)) = row else { continue };
        if owner == account {
            continue;
        }
        // An existing membership keeps whichever role is higher.
        let existing: Option<(String,)> =
            sqlx::query_as("select role from members where item_kind = $1 and item_id = $2 and account = $3 for update")
                .bind(kind.as_str())
                .bind(&item_id)
                .bind(account)
                .fetch_optional(&mut *tx)
                .await?;
        match existing.and_then(|e| Role::parse(&e.0)) {
            Some(current) if current >= role => continue,
            Some(_) => {
                sqlx::query("update members set role = $4 where item_kind = $1 and item_id = $2 and account = $3")
                    .bind(kind.as_str())
                    .bind(&item_id)
                    .bind(account)
                    .bind(role.as_str())
                    .execute(&mut *tx)
                    .await?;
            }
            None => {
                sqlx::query("insert into members (item_kind, item_id, account, role, invited_by) values ($1, $2, $3, $4, $5)")
                    .bind(kind.as_str())
                    .bind(&item_id)
                    .bind(account)
                    .bind(role.as_str())
                    .bind(invited_by)
                    .execute(&mut *tx)
                    .await?;
                if notify_them {
                    tings.push((kind, item_id.clone(), title, role, invited_by));
                }
            }
        }
        made += 1;
    }
    for (kind, item_id, title, role, invited_by) in tings {
        let by = state.accounts.get(invited_by).await;
        queue_shared(&mut tx, state, kind, &item_id, &title, role, account, &by).await?;
    }
    tx.commit().await?;
    state.notify.wake();
    Ok(made)
}

// ------------------------------------------------------------------------------------------------
// Changing roles, removing, leaving
// ------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct RoleBody {
    role: Option<String>,
}

fn parse_member(account: &str) -> ApiResult<Uuid> {
    Uuid::parse_str(account.trim()).map_err(|_| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("{account} isn't a member's uuid."),
            "Members are named by their account uuid, as GET …/sharing shows them.",
        )
    })
}

async fn direct_role(state: &AppState, kind: Kind, item_id: &str, account: Uuid) -> ApiResult<Option<Role>> {
    let r: Option<(String,)> = sqlx::query_as("select role from members where item_kind = $1 and item_id = $2 and account = $3")
        .bind(kind.as_str())
        .bind(item_id)
        .bind(account)
        .fetch_optional(&state.db)
        .await?;
    Ok(r.and_then(|r| Role::parse(&r.0)))
}

/// Why an account with no direct membership can't be changed here: they have access through a folder, or none.
async fn not_direct(state: &AppState, item: &Item, account: Uuid, me: bool) -> ApiError {
    let chain = perms::ancestors(state, item.parent.as_deref()).await.unwrap_or_default();
    let ids: Vec<String> = chain.iter().map(|f| f.id.clone()).collect();
    let via: Option<(String,)> = sqlx::query_as(
        "select item_id from members where item_kind = 'folder' and account = $1 and item_id = any($2) limit 1",
    )
    .bind(account)
    .bind(&ids)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);
    match via.and_then(|v| chain.into_iter().find(|f| f.id == v.0)) {
        Some(folder) if me => ApiError::conflict(
            format!("Your access to {} comes from the folder \"{}\".", item.what(), folder.name),
            format!("Leave the folder \"{}\" instead.", folder.name),
        ),
        Some(folder) => ApiError::conflict(
            format!("Their access to {} comes from the folder \"{}\", not from {} itself.", item.what(), folder.name, item.what()),
            format!("Change it on the folder \"{}\", or invite them here directly with a higher role.", folder.name),
        ),
        None if me => ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("You aren't a member of {}, so there is nothing to leave.", item.what()),
            "Your access may come from its public link, which can't be left.",
        ),
        None => ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("That account isn't a member of {}.", item.what()),
            "GET …/sharing lists its members.",
        ),
    }
}

async fn change(state: AppState, caller: Caller, kind: Kind, id: String, account: String, body: RoleBody) -> ApiResult<Json<Value>> {
    let item = item(&state, &caller, kind, &id).await?;
    item.require_share()?;
    let account = parse_member(&account)?;
    let Some(role) = body.role.as_deref() else {
        return Err(ApiError::invalid_field("role", "Give the new role: editor, commenter or viewer."));
    };
    let role = shareable(Some(role), "role")?;
    not_above(&item, role)?;
    if account == item.owner {
        return Err(ApiError::conflict(
            format!("That account owns {}, so it has no role to change.", item.what()),
            "Ownership only moves by a transfer (calc transfer).",
        ));
    }
    if direct_role(&state, kind, &item.id, account).await?.is_none() {
        return Err(not_direct(&state, &item, account, account == caller.uuid).await);
    }
    sqlx::query("update members set role = $4 where item_kind = $1 and item_id = $2 and account = $3")
        .bind(kind.as_str())
        .bind(&item.id)
        .bind(account)
        .bind(role.as_str())
        .execute(&state.db)
        .await?;
    let item = self::item(&state, &caller, kind, &id).await?;
    Ok(Json(sharing_json(&state, &caller, &item).await?))
}

async fn sheet_change(State(state): State<AppState>, caller: Caller, Path((sheet, account)): Path<(String, String)>, JsonBody(body): JsonBody<RoleBody>) -> ApiResult<Json<Value>> {
    change(state, caller, Kind::Sheet, sheet, account, body).await
}

async fn folder_change(State(state): State<AppState>, caller: Caller, Path((folder, account)): Path<(String, String)>, JsonBody(body): JsonBody<RoleBody>) -> ApiResult<Json<Value>> {
    change(state, caller, Kind::Folder, folder, account, body).await
}

async fn remove(state: AppState, caller: Caller, kind: Kind, id: String, account: String) -> ApiResult<StatusCode> {
    let item = item(&state, &caller, kind, &id).await?;
    let account = parse_member(&account)?;
    let leaving = account == caller.uuid;
    if leaving && account == item.owner {
        return Err(ApiError::conflict(
            format!("You own {}, so you can't leave it.", item.what()),
            "Transfer it to someone else first (calc transfer), or move it to the trash.",
        ));
    }
    if !leaving {
        item.require_share()?;
    }
    if direct_role(&state, kind, &item.id, account).await?.is_none() {
        return Err(not_direct(&state, &item, account, leaving).await);
    }
    sqlx::query("delete from members where item_kind = $1 and item_id = $2 and account = $3")
        .bind(kind.as_str())
        .bind(&item.id)
        .bind(account)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn sheet_remove(State(state): State<AppState>, caller: Caller, Path((sheet, account)): Path<(String, String)>) -> ApiResult<StatusCode> {
    remove(state, caller, Kind::Sheet, sheet, account).await
}

async fn folder_remove(State(state): State<AppState>, caller: Caller, Path((folder, account)): Path<(String, String)>) -> ApiResult<StatusCode> {
    remove(state, caller, Kind::Folder, folder, account).await
}

async fn uninvite(state: AppState, caller: Caller, kind: Kind, id: String, email: String) -> ApiResult<StatusCode> {
    let item = item(&state, &caller, kind, &id).await?;
    item.require_share()?;
    let res = sqlx::query("delete from invites where item_kind = $1 and item_id = $2 and email = $3::citext")
        .bind(kind.as_str())
        .bind(&item.id)
        .bind(email.trim())
        .execute(&state.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("There is no invite to {} for {}.", email.trim(), item.what()),
            "GET …/sharing lists its invites.",
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn sheet_uninvite(State(state): State<AppState>, caller: Caller, Path((sheet, email)): Path<(String, String)>) -> ApiResult<StatusCode> {
    uninvite(state, caller, Kind::Sheet, sheet, email).await
}

async fn folder_uninvite(State(state): State<AppState>, caller: Caller, Path((folder, email)): Path<(String, String)>) -> ApiResult<StatusCode> {
    uninvite(state, caller, Kind::Folder, folder, email).await
}

// ------------------------------------------------------------------------------------------------
// Public links
// ------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct PublicBody {
    enabled: Option<bool>,
    access: Option<String>,
}

async fn sheet_public(State(state): State<AppState>, caller: Caller, Path(sheet): Path<String>, JsonBody(body): JsonBody<PublicBody>) -> ApiResult<Json<Value>> {
    let item = item(&state, &caller, Kind::Sheet, &sheet).await?;
    item.require_share()?;
    let (was_enabled, was_access) = item.public.clone().unwrap_or((false, "view".into()));
    let enabled = body.enabled.unwrap_or(was_enabled);
    let access = body.access.as_deref().map(|a| a.trim().to_ascii_lowercase()).unwrap_or(was_access);
    let Some(link_role) = Role::of_public_access(&access) else {
        return Err(ApiError::invalid_field("access", format!("\"{access}\" isn't a public link access; use view, comment or edit.")));
    };
    if enabled {
        not_above(&item, link_role)?;
    }
    sqlx::query("update sheets set public_enabled = $2, public_access = $3, updated_at = now(), updated_by = $4 where id = $1")
        .bind(&item.id)
        .bind(enabled)
        .bind(&access)
        .bind(caller.uuid)
        .execute(&state.db)
        .await?;
    Ok(Json(json!({"enabled": enabled, "access": access, "url": state.cfg.sheet_url(&item.id)})))
}

async fn folder_public(State(state): State<AppState>, caller: Caller, Path(folder): Path<String>) -> ApiResult<Json<Value>> {
    item(&state, &caller, Kind::Folder, &folder).await?;
    Err(ApiError::bad_request(
        "Folders can't be made public; only sheets have public links.",
        "Make the sheets inside it public one by one, or share the folder with the Carbons and Silicons who need it.",
    ))
}
