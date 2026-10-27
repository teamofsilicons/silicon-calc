//! Who may do what to a sheet or folder: the owner, members (directly or through any folder above), and the
//! public link. The highest role wins.
//!
//! Everything in a folder belongs to the folder's owner (a sheet or folder made inside someone else's shared folder
//! belongs to that folder's owner), so the owner of a sheet is also the owner of every folder above it.
//! Something in the trash, or inside a folder in the trash, is only its owner's until it is restored.

use std::collections::HashMap;

use serde::Serialize;
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    auth::Caller,
    error::{ApiError, ApiResult},
    state::AppState,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Viewer,
    Commenter,
    Editor,
    Owner,
}

impl Role {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "viewer" => Self::Viewer,
            "commenter" => Self::Commenter,
            "editor" => Self::Editor,
            "owner" => Self::Owner,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Commenter => "commenter",
            Self::Editor => "editor",
            Self::Owner => "owner",
        }
    }

    pub fn can_comment(self) -> bool {
        self >= Self::Commenter
    }

    pub fn can_edit(self) -> bool {
        self >= Self::Editor
    }

    /// Inviting, changing roles and the public link.
    pub fn can_share(self) -> bool {
        self >= Self::Editor
    }

    /// A role that can be given to someone (`editor`, `commenter`, `viewer`; never `owner`).
    pub fn parse_shareable(s: &str) -> Option<Self> {
        match Self::parse(s.trim().to_ascii_lowercase().as_str())? {
            Self::Owner => None,
            r => Some(r),
        }
    }

    /// The role a public link's access gives (`view`, `comment`, `edit`).
    pub fn of_public_access(access: &str) -> Option<Self> {
        Some(match access {
            "view" => Self::Viewer,
            "comment" => Self::Commenter,
            "edit" => Self::Editor,
            _ => return None,
        })
    }

    /// "a viewer", "an editor": for messages.
    pub fn with_article(self) -> &'static str {
        match self {
            Self::Viewer => "a viewer",
            Self::Commenter => "a commenter",
            Self::Editor => "an editor",
            Self::Owner => "the owner",
        }
    }
}

/// The columns `SheetRow` reads, for `select {SHEET_COLUMNS} from sheets …`.
pub const SHEET_COLUMNS: &str = "id, title, folder_id, owner, created_by, created_at, updated_at, updated_by, trashed_at, \
                                 public_enabled, public_access, require_email, email_gate_version";

/// The columns `FolderRow` reads.
pub const FOLDER_COLUMNS: &str = "id, name, parent_id, owner, created_by, created_at, trashed_at";

#[derive(Clone, Debug, FromRow)]
pub struct SheetRow {
    pub id: String,
    pub title: String,
    pub folder_id: Option<String>,
    pub owner: Uuid,
    pub created_by: Uuid,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub updated_by: Uuid,
    pub trashed_at: Option<OffsetDateTime>,
    pub public_enabled: bool,
    pub public_access: String,
    pub require_email: bool,
    pub email_gate_version: i64,
}

#[derive(Clone, Debug, FromRow)]
pub struct FolderRow {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub owner: Uuid,
    pub created_by: Uuid,
    pub created_at: OffsetDateTime,
    pub trashed_at: Option<OffsetDateTime>,
}

/// A sheet and the caller's role on it.
#[derive(Clone, Debug)]
pub struct SheetAccess {
    pub sheet: SheetRow,
    pub role: Role,
    /// The sheet or a folder above it is in the trash.
    pub in_trash: bool,
}

#[derive(Clone, Debug)]
pub struct FolderAccess {
    pub folder: FolderRow,
    pub role: Role,
    pub in_trash: bool,
}

/// What the folders above (or including) an item say about one account.
#[derive(Clone, Debug, Default)]
struct Chain {
    /// The best member role on any of them.
    role: Option<Role>,
    /// One of them is in the trash.
    trashed: bool,
    /// The account owns one of them.
    owns_one: bool,
}

/// The public link's role for an account (signed in or not).
fn public_role(sheet: &SheetRow, signed_in: bool) -> Option<Role> {
    if !sheet.public_enabled {
        return None;
    }
    Some(match sheet.public_access.as_str() {
        // Commenting and editing through the link need a sign-in, so every change has an account behind it.
        "edit" if signed_in => Role::Editor,
        "comment" if signed_in => Role::Commenter,
        _ => Role::Viewer,
    })
}

/// The one rule for a sheet: owner, else the best of direct membership, folder membership and the public link.
/// Something in the trash is only its owner's.
fn decide_sheet(sheet: &SheetRow, me: Option<Uuid>, direct: Option<Role>, chain: &Chain) -> (Option<Role>, bool) {
    let in_trash = sheet.trashed_at.is_some() || chain.trashed;
    let mut role = match me {
        Some(me) if sheet.owner == me => Some(Role::Owner),
        Some(_) => direct.max(chain.role),
        None => None,
    };
    role = role.max(public_role(sheet, me.is_some()));
    if in_trash && role != Some(Role::Owner) {
        role = None;
    }
    (role, in_trash)
}

fn decide_folder(folder: &FolderRow, me: Uuid, chain: &Chain) -> (Option<Role>, bool) {
    let in_trash = chain.trashed;
    let mut role = if folder.owner == me || chain.owns_one { Some(Role::Owner) } else { chain.role };
    if in_trash && role != Some(Role::Owner) {
        role = None;
    }
    (role, in_trash)
}

/// For each sheet, what the folders above it say about `me`.
async fn sheet_chains(state: &AppState, me: Option<Uuid>, ids: &[String]) -> ApiResult<HashMap<String, Chain>> {
    let rows: Vec<(String, bool, Uuid, Option<String>)> = sqlx::query_as(
        "with recursive up(sheet_id, folder_id, depth) as (
             select s.id, s.folder_id, 0 from sheets s where s.id = any($1) and s.folder_id is not null
             union all
             select u.sheet_id, f.parent_id, u.depth + 1 from up u join folders f on f.id = u.folder_id
             where f.parent_id is not null and u.depth < 100
         )
         select u.sheet_id, f.trashed_at is not null, f.owner, m.role
         from up u join folders f on f.id = u.folder_id and f.deleted_at is null
         left join members m on m.item_kind = 'folder' and m.item_id = f.id and m.account = $2",
    )
    .bind(ids)
    .bind(me)
    .fetch_all(&state.db)
    .await?;
    let mut out: HashMap<String, Chain> = HashMap::new();
    for (sheet, trashed, owner, role) in rows {
        let c = out.entry(sheet).or_default();
        c.trashed |= trashed;
        c.owns_one |= Some(owner) == me;
        c.role = c.role.max(role.as_deref().and_then(Role::parse));
    }
    Ok(out)
}

/// For each folder, what it and the folders above it say about `me`.
async fn folder_chains(state: &AppState, me: Uuid, ids: &[String]) -> ApiResult<HashMap<String, Chain>> {
    let rows: Vec<(String, bool, Uuid, Option<String>)> = sqlx::query_as(
        "with recursive up(start, id, depth) as (
             select f.id, f.id, 0 from folders f where f.id = any($1)
             union all
             select u.start, f.parent_id, u.depth + 1 from up u join folders f on f.id = u.id
             where f.parent_id is not null and u.depth < 100
         )
         select u.start, f.trashed_at is not null, f.owner, m.role
         from up u join folders f on f.id = u.id and f.deleted_at is null
         left join members m on m.item_kind = 'folder' and m.item_id = f.id and m.account = $2",
    )
    .bind(ids)
    .bind(me)
    .fetch_all(&state.db)
    .await?;
    let mut out: HashMap<String, Chain> = HashMap::new();
    for (start, trashed, owner, role) in rows {
        let c = out.entry(start).or_default();
        c.trashed |= trashed;
        c.owns_one |= owner == me;
        c.role = c.role.max(role.as_deref().and_then(Role::parse));
    }
    Ok(out)
}

async fn direct_roles(state: &AppState, kind: &str, me: Uuid, ids: &[String]) -> ApiResult<HashMap<String, Role>> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("select item_id, role from members where item_kind = $1 and account = $2 and item_id = any($3)")
            .bind(kind)
            .bind(me)
            .bind(ids)
            .fetch_all(&state.db)
            .await?;
    Ok(rows.into_iter().filter_map(|(id, r)| Role::parse(&r).map(|r| (id, r))).collect())
}

/// Every given sheet's role for `me` (signed in or not) and whether it is in the trash. Sheets `me` has no access
/// to are left out.
pub async fn sheet_roles(state: &AppState, me: Option<Uuid>, sheets: &[SheetRow]) -> ApiResult<HashMap<String, (Role, bool)>> {
    if sheets.is_empty() {
        return Ok(HashMap::new());
    }
    let ids: Vec<String> = sheets.iter().map(|s| s.id.clone()).collect();
    let chains = sheet_chains(state, me, &ids).await?;
    let direct = match me {
        Some(me) => direct_roles(state, "sheet", me, &ids).await?,
        None => HashMap::new(),
    };
    let none = Chain::default();
    Ok(sheets
        .iter()
        .filter_map(|s| {
            let (role, trash) = decide_sheet(s, me, direct.get(&s.id).copied(), chains.get(&s.id).unwrap_or(&none));
            role.map(|r| (s.id.clone(), (r, trash)))
        })
        .collect())
}

/// Every given folder's role for `me` and whether it is in the trash. Folders `me` has no access to are left out.
pub async fn folder_roles(state: &AppState, me: Uuid, folders: &[FolderRow]) -> ApiResult<HashMap<String, (Role, bool)>> {
    if folders.is_empty() {
        return Ok(HashMap::new());
    }
    let ids: Vec<String> = folders.iter().map(|f| f.id.clone()).collect();
    let chains = folder_chains(state, me, &ids).await?;
    let none = Chain::default();
    Ok(folders
        .iter()
        .filter_map(|f| {
            let (role, trash) = decide_folder(f, me, chains.get(&f.id).unwrap_or(&none));
            role.map(|r| (f.id.clone(), (r, trash)))
        })
        .collect())
}

/// A sheet that isn't deleted for good.
pub async fn load_sheet(state: &AppState, sheet_id: &str) -> ApiResult<Option<SheetRow>> {
    if !crate::ids::looks_like_id(sheet_id) {
        return Ok(None);
    }
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select {SHEET_COLUMNS} from sheets where id = $1 and deleted_at is null"
    )))
    .bind(sheet_id)
    .fetch_optional(&state.db)
    .await?)
}

/// A folder that isn't deleted for good.
pub async fn load_folder(state: &AppState, folder_id: &str) -> ApiResult<Option<FolderRow>> {
    if !crate::ids::looks_like_id(folder_id) {
        return Ok(None);
    }
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select {FOLDER_COLUMNS} from folders where id = $1 and deleted_at is null"
    )))
    .bind(folder_id)
    .fetch_optional(&state.db)
    .await?)
}

/// The folder and every folder above it, nearest first.
pub async fn ancestors(state: &AppState, folder_id: Option<&str>) -> ApiResult<Vec<FolderRow>> {
    let Some(folder_id) = folder_id else { return Ok(Vec::new()) };
    let rows: Vec<FolderRow> = sqlx::query_as(
        "with recursive chain(id, depth) as (
             select id, 0 from folders where id = $1 and deleted_at is null
             union all
             select f.parent_id, c.depth + 1 from chain c join folders f on f.id = c.id
             where f.parent_id is not null and c.depth < 100
         )
         select f.id, f.name, f.parent_id, f.owner, f.created_by, f.created_at, f.trashed_at
         from chain c join folders f on f.id = c.id and f.deleted_at is null order by c.depth",
    )
    .bind(folder_id)
    .fetch_all(&state.db)
    .await?;
    Ok(rows)
}

/// The caller's access to a sheet. Answers 404 when they have none (so ids can't be probed), and when the sheet
/// is in the trash for anyone but its owner; 401 when nobody is signed in and the sheet isn't public.
pub async fn sheet_access(state: &AppState, caller: Option<&Caller>, sheet_id: &str) -> ApiResult<SheetAccess> {
    let me = caller.map(|c| c.uuid);
    let denied = || if me.is_some() { ApiError::not_found("This sheet") } else { ApiError::signed_out() };
    let Some(sheet) = load_sheet(state, sheet_id).await? else { return Err(denied()) };
    let mut roles = sheet_roles(state, me, std::slice::from_ref(&sheet)).await?;
    let Some((role, in_trash)) = roles.remove(&sheet.id) else { return Err(denied()) };
    Ok(SheetAccess { sheet, role, in_trash })
}

/// `account`'s role on a sheet, or None when they can't open it (no access, or it's in the trash and they aren't
/// its owner). For deciding who hears about something.
pub async fn sheet_role_of(state: &AppState, account: Uuid, sheet_id: &str) -> ApiResult<Option<Role>> {
    let Some(sheet) = load_sheet(state, sheet_id).await? else { return Ok(None) };
    Ok(sheet_roles(state, Some(account), std::slice::from_ref(&sheet)).await?.remove(&sheet.id).map(|r| r.0))
}

/// The caller's access to a folder.
pub async fn folder_access(state: &AppState, caller: &Caller, folder_id: &str) -> ApiResult<FolderAccess> {
    folder_access_for(state, caller.uuid, folder_id).await
}

/// `account`'s access to a folder (404 when they have none).
pub async fn folder_access_for(state: &AppState, account: Uuid, folder_id: &str) -> ApiResult<FolderAccess> {
    let Some(folder) = load_folder(state, folder_id).await? else { return Err(ApiError::not_found("This folder")) };
    let mut roles = folder_roles(state, account, std::slice::from_ref(&folder)).await?;
    let Some((role, in_trash)) = roles.remove(&folder.id) else { return Err(ApiError::not_found("This folder")) };
    Ok(FolderAccess { folder, role, in_trash })
}

/// `account`'s role on a folder, or None.
pub async fn folder_role_of(state: &AppState, account: Uuid, folder_id: &str) -> ApiResult<Option<Role>> {
    match folder_access_for(state, account, folder_id).await {
        Ok(a) => Ok(Some(a.role)),
        Err(e) if e.code == "not_found" => Ok(None),
        Err(e) => Err(e),
    }
}

/// Every sheet `account` can read through ownership or membership (directly or through any folder above), that
/// isn't in the trash or deleted. Public sheets they merely have the link to are not listed anywhere.
pub async fn readable_sheet_ids(state: &AppState, account: Uuid) -> ApiResult<Vec<String>> {
    let ids: Vec<(String,)> = sqlx::query_as(
        "with recursive reach(id) as (
             select f.id from members m join folders f on f.id = m.item_id
             where m.item_kind = 'folder' and m.account = $1 and f.deleted_at is null
             union
             select f.id from folders f join reach r on f.parent_id = r.id where f.deleted_at is null
         ),
         cand(id, folder_id) as (
             select id, folder_id from sheets where owner = $1 and deleted_at is null and trashed_at is null
             union
             select s.id, s.folder_id from members m join sheets s on s.id = m.item_id
             where m.item_kind = 'sheet' and m.account = $1 and s.deleted_at is null and s.trashed_at is null
             union
             select s.id, s.folder_id from reach r join sheets s on s.folder_id = r.id
             where s.deleted_at is null and s.trashed_at is null
         ),
         up(sheet_id, folder_id, depth) as (
             select id, folder_id, 0 from cand where folder_id is not null
             union all
             select u.sheet_id, f.parent_id, u.depth + 1 from up u join folders f on f.id = u.folder_id
             where f.parent_id is not null and u.depth < 100
         )
         select c.id from cand c
         where not exists (
             select 1 from up u join folders f on f.id = u.folder_id where u.sheet_id = c.id and f.trashed_at is not null
         )",
    )
    .bind(account)
    .fetch_all(&state.db)
    .await?;
    Ok(ids.into_iter().map(|r| r.0).collect())
}

/// When a sheet or folder went into the trash: its own time, or that of the nearest folder above it in the trash.
pub async fn trashed_since(state: &AppState, own: Option<OffsetDateTime>, folder_id: Option<&str>) -> ApiResult<Option<OffsetDateTime>> {
    if own.is_some() {
        return Ok(own);
    }
    Ok(ancestors(state, folder_id).await?.into_iter().find_map(|f| f.trashed_at))
}

fn trash_conflict(what: &str, title: &str) -> ApiError {
    ApiError::conflict(
        format!("The {what} \"{title}\" is in the trash, so it can't be changed."),
        format!("Restore it from the trash first (calc restore), then try again."),
    )
}

impl SheetAccess {
    pub fn require_comment(&self) -> ApiResult<()> {
        if self.role.can_comment() {
            Ok(())
        } else {
            Err(ApiError::not_allowed(
                format!("You can view \"{}\" but not comment on it.", self.sheet.title),
                "Ask its owner to share it with you as a commenter or editor.",
            ))
        }
    }

    pub fn require_edit(&self) -> ApiResult<()> {
        if self.in_trash {
            return Err(ApiError::conflict(
                format!("\"{}\" is in the trash.", self.sheet.title),
                "Restore it from the trash first (calc restore).",
            ));
        }
        if self.role.can_edit() {
            Ok(())
        } else {
            Err(ApiError::not_allowed(
                format!("You can't edit \"{}\": you are {} on it.", self.sheet.title, self.role.with_article()),
                "Ask its owner to share it with you as an editor.",
            ))
        }
    }

    pub fn require_owner(&self) -> ApiResult<()> {
        if self.role == Role::Owner {
            Ok(())
        } else {
            Err(ApiError::not_allowed(
                format!("Only the owner of \"{}\" can do that.", self.sheet.title),
                "Ask its owner.",
            ))
        }
    }

    /// Inviting, changing roles and the public link: the owner and editors, outside the trash.
    pub fn require_share(&self) -> ApiResult<()> {
        if !self.role.can_share() {
            return Err(ApiError::not_allowed(
                format!("You can't share \"{}\": you are {} on it, and only its owner and editors can.", self.sheet.title, self.role.with_article()),
                "Ask its owner or an editor to invite them.",
            ));
        }
        self.require_live()
    }

    /// Not in the trash.
    pub fn require_live(&self) -> ApiResult<()> {
        if self.in_trash { Err(trash_conflict("sheet", &self.sheet.title)) } else { Ok(()) }
    }
}

impl FolderAccess {
    /// Renaming, moving, adding sheets and folders inside: the owner and editors, outside the trash.
    pub fn require_edit(&self) -> ApiResult<()> {
        if !self.role.can_edit() {
            return Err(ApiError::not_allowed(
                format!("You can't change the folder \"{}\": you are {} on it.", self.folder.name, self.role.with_article()),
                "Ask its owner to share it with you as an editor.",
            ));
        }
        self.require_live()
    }

    pub fn require_owner(&self) -> ApiResult<()> {
        if self.role == Role::Owner {
            Ok(())
        } else {
            Err(ApiError::not_allowed(
                format!("Only the owner of the folder \"{}\" can do that.", self.folder.name),
                "Ask its owner.",
            ))
        }
    }

    pub fn require_share(&self) -> ApiResult<()> {
        if !self.role.can_share() {
            return Err(ApiError::not_allowed(
                format!("You can't share the folder \"{}\": you are {} on it, and only its owner and editors can.", self.folder.name, self.role.with_article()),
                "Ask its owner or an editor to invite them.",
            ));
        }
        self.require_live()
    }

    pub fn require_live(&self) -> ApiResult<()> {
        if self.in_trash { Err(trash_conflict("folder", &self.folder.name)) } else { Ok(()) }
    }
}
