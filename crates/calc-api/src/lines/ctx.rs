//! Which line a request is about and what the caller may do on it, and how line routes are registered (every route
//! under `{line}` exists for the main sheet and for each copy, docs/API.md).

use axum::{Router, routing::MethodRouter};
use serde::Deserialize;

use super::{LineHandle, LineRow, line_for};
use crate::{
    auth::Caller,
    error::{ApiError, ApiResult},
    perms::{self, SheetAccess},
    state::AppState,
};

/// `{sheet}` or `{sheet}/copies/{copy}`.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct LinePath {
    pub sheet: String,
    #[serde(default)]
    pub copy: Option<String>,
}

/// A line route with a tab: `{line}/tabs/{tab}/…`.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct TabPath {
    pub sheet: String,
    #[serde(default)]
    pub copy: Option<String>,
    pub tab: String,
}

/// `{line}/tabs/{tab}/cells/{a1}`.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct CellPath {
    pub sheet: String,
    #[serde(default)]
    pub copy: Option<String>,
    pub tab: String,
    pub a1: String,
}

/// `{line}/versions/{v}/…`.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct VersionPath {
    pub sheet: String,
    #[serde(default)]
    pub copy: Option<String>,
    pub v: String,
}

/// `{line}/versions/{v}/tabs/{tab}/cells`.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct VersionTabPath {
    pub sheet: String,
    #[serde(default)]
    pub copy: Option<String>,
    pub v: String,
    pub tab: String,
}

/// Registers `mr` for the main line (`/sheets/{sheet}{suffix}`) and for copies (`/sheets/{sheet}/copies/{copy}{suffix}`).
pub(crate) fn both(router: Router<AppState>, suffix: &str, mr: MethodRouter<AppState>) -> Router<AppState> {
    router
        .route(&format!("/sheets/{{sheet}}{suffix}"), mr.clone())
        .route(&format!("/sheets/{{sheet}}/copies/{{copy}}{suffix}"), mr)
}

/// A request's sheet and line, and the caller's access.
pub(crate) struct LineCtx {
    pub access: SheetAccess,
    pub line: LineRow,
}

impl LineCtx {
    /// Checks the caller can open the sheet (404 otherwise, 401 for nobody signed in on a sheet that isn't public)
    /// and that the copy belongs to it.
    pub async fn load(state: &AppState, caller: Option<&Caller>, sheet: &str, copy: Option<&str>) -> ApiResult<Self> {
        let access = perms::sheet_access(state, caller, sheet).await?;
        let line = line_for(state, &access.sheet.id, copy).await?;
        Ok(Self { access, line })
    }

    pub async fn of(state: &AppState, caller: Option<&Caller>, p: &LinePath) -> ApiResult<Self> {
        Self::load(state, caller, &p.sheet, p.copy.as_deref()).await
    }

    /// The copy's id, or None on the main line.
    pub fn copy_id(&self) -> Option<&str> {
        (!self.line.is_main).then_some(self.line.id.as_str())
    }

    /// The line's open task.
    pub async fn handle(&self, state: &AppState) -> ApiResult<LineHandle> {
        state.lines.open(state, &self.line.id).await
    }

    /// The line takes changes: it is the main line or an open copy.
    pub fn require_open_line(&self) -> ApiResult<()> {
        if self.line.status == "open" {
            return Ok(());
        }
        let name = self.line.name.clone().unwrap_or_default();
        Err(ApiError::conflict(
            format!(
                "The copy \"{name}\" was {}, so it can be read but not changed.",
                if self.line.status == "merged" { "merged and closed" } else { "deleted" }
            ),
            "Make a new copy (calc copies new) to keep working on it.",
        ))
    }

    /// Editing: editors and up, outside the trash, on a line that takes changes.
    pub fn require_edit(&self) -> ApiResult<()> {
        self.access.require_edit()?;
        self.require_open_line()
    }
}
