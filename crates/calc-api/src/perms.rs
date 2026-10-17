//! Who may do what to a sheet or folder: the owner, members (directly or through any folder above), and the
//! public link. The highest role wins.

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
}

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


/// The best member role `account` has on `folder_id` or any folder above it, and whether any of them is trashed.
async fn folder_chain(state: &AppState, folder_id: &str, account: Option<Uuid>) -> ApiResult<(Option<Role>, bool)> {
    let rows: Vec<(Option<String>, bool)> = sqlx::query_as(
        "with recursive chain(id, parent_id, trashed) as (
             select id, parent_id, trashed_at is not null from folders where id = $1 and deleted_at is null
             union all
             select f.id, f.parent_id, f.trashed_at is not null from folders f join chain c on f.id = c.parent_id
             where f.deleted_at is null
         )
         select m.role, c.trashed from chain c
         left join members m on m.item_kind = 'folder' and m.item_id = c.id and m.account = $2",
    )
    .bind(folder_id)
    .bind(account)
    .fetch_all(&state.db)
    .await?;
    let trashed = rows.iter().any(|r| r.1);
    let role = rows.iter().filter_map(|r| r.0.as_deref().and_then(Role::parse)).max();
    Ok((role, trashed))
}

/// The owners of every folder above `folder_id` see everything inside, as owners of the folder's contents.
async fn owns_folder_above(state: &AppState, folder_id: &str, account: Uuid) -> ApiResult<bool> {
    let n: Option<(i64,)> = sqlx::query_as(
        "with recursive chain(id, parent_id, owner) as (
             select id, parent_id, owner from folders where id = $1 and deleted_at is null
             union all
             select f.id, f.parent_id, f.owner from folders f join chain c on f.id = c.parent_id where f.deleted_at is null
         ) select count(*) from chain where owner = $2",
    )
    .bind(folder_id)
    .bind(account)
    .fetch_optional(&state.db)
    .await?;
    Ok(n.is_some_and(|n| n.0 > 0))
}

/// The caller's access to a sheet. Answers 404 when they have none (so ids can't be probed), and when the sheet
/// is in the trash for anyone but its owner.
pub async fn sheet_access(state: &AppState, caller: Option<&Caller>, sheet_id: &str) -> ApiResult<SheetAccess> {
    if !crate::ids::looks_like_id(sheet_id) {
        return Err(ApiError::not_found("This sheet"));
    }
    let sheet: Option<SheetRow> =
        sqlx::query_as(
            "select id, title, folder_id, owner, created_by, created_at, updated_at, updated_by, trashed_at,
                    public_enabled, public_access
             from sheets where id = $1 and deleted_at is null",
        )
            .bind(sheet_id)
            .fetch_optional(&state.db)
            .await?;
    let Some(sheet) = sheet else { return Err(ApiError::not_found("This sheet")) };
    let me = caller.map(|c| c.uuid);
    let (folder_role, folder_trashed) = match &sheet.folder_id {
        Some(f) => folder_chain(state, f, me).await?,
        None => (None, false),
    };
    let in_trash = sheet.trashed_at.is_some() || folder_trashed;
    let mut role: Option<Role> = None;
    if let Some(me) = me {
        if sheet.owner == me {
            role = Some(Role::Owner);
        } else {
            let direct: Option<(String,)> =
                sqlx::query_as("select role from members where item_kind = 'sheet' and item_id = $1 and account = $2")
                    .bind(sheet_id)
                    .bind(me)
                    .fetch_optional(&state.db)
                    .await?;
            role = direct.and_then(|d| Role::parse(&d.0)).max(folder_role);
        }
    }
    if sheet.public_enabled {
        let public = match sheet.public_access.as_str() {
            // Commenting and editing through the link need a sign-in, so every change has an account behind it.
            "edit" if me.is_some() => Role::Editor,
            "comment" if me.is_some() => Role::Commenter,
            _ => Role::Viewer,
        };
        role = role.max(Some(public));
    }
    let Some(role) = role else { return Err(ApiError::not_found("This sheet")) };
    if in_trash && role != Role::Owner {
        return Err(ApiError::not_found("This sheet"));
    }
    Ok(SheetAccess { sheet, role, in_trash })
}

/// The caller's access to a folder.
pub async fn folder_access(state: &AppState, caller: &Caller, folder_id: &str) -> ApiResult<FolderAccess> {
    if !crate::ids::looks_like_id(folder_id) {
        return Err(ApiError::not_found("This folder"));
    }
    let folder: Option<FolderRow> = sqlx::query_as(
        "select id, name, parent_id, owner, created_by, created_at, trashed_at from folders where id = $1 and deleted_at is null",
    )
    .bind(folder_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(folder) = folder else { return Err(ApiError::not_found("This folder")) };
    let (member, trashed) = folder_chain(state, folder_id, Some(caller.uuid)).await?;
    let role = if folder.owner == caller.uuid || owns_folder_above(state, folder_id, caller.uuid).await? {
        Some(Role::Owner)
    } else {
        member
    };
    let Some(role) = role else { return Err(ApiError::not_found("This folder")) };
    if trashed && role != Role::Owner {
        return Err(ApiError::not_found("This folder"));
    }
    Ok(FolderAccess { folder, role, in_trash: trashed })
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
                format!("You can't edit \"{}\": you are a {} on it.", self.sheet.title, self.role.as_str()),
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
}
