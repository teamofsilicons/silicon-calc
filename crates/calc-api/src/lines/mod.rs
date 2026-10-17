//! Lines: a sheet's main line and its copies. Each open line is one task owning its `Workbook`; it applies
//! operations in order, writes them to `ops`, snapshots every so often, recalculates and tells viewers.
//! Routes: tabs, cells, edits, events, presence, versions, compare, restore, copies. See docs/API.md.
//!
//! The functions marked "contract" are called by other modules; keep their signatures.

use std::sync::Arc;

use axum::Router;
use calc_engine::{Workbook, snapshot};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{error::ApiResult, state::AppState};

/// The open lines.
#[derive(Clone, Default)]
pub struct Registry {
    _inner: Arc<()>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Contract: the handle of an open line, loading it (latest snapshot + operations after it) if needed.
    pub async fn open(&self, _state: &AppState, line_id: &str) -> ApiResult<LineHandle> {
        Err(not_built(line_id))
    }

    /// Saves every open line (on shutdown).
    pub async fn shutdown(&self, _state: &AppState) {}
}

fn not_built(what: &str) -> crate::error::ApiError {
    crate::error::ApiError::unavailable(format!("Lines are not built yet ({what})."), "Build the lines work item.")
}

/// What kind of change an operation is, as history shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpKind {
    Create,
    Edit,
    Import,
    Code,
    Merge,
    Restore,
    Undo,
    Redo,
}

impl OpKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Edit => "edit",
            Self::Import => "import",
            Self::Code => "code",
            Self::Merge => "merge",
            Self::Restore => "restore",
            Self::Undo => "undo",
            Self::Redo => "redo",
        }
    }
}

/// Contract: a change to apply to a line as one version.
pub struct ApplyRequest {
    pub author: Uuid,
    pub kind: OpKind,
    /// What history says ("imported sales.csv into Sales"); made from the operation when None.
    pub summary: Option<String>,
    /// The version the caller was looking at, for reporting overwritten cells.
    pub base_version: Option<i64>,
    pub client_id: Option<String>,
    /// Builds the operation from the line's current workbook, inside the line's task (so ids for new rows and
    /// columns are allocated deterministically). `Ok(None)`: nothing to do (no new version).
    pub build: Box<dyn FnOnce(&Workbook) -> ApiResult<Option<calc_engine::Op>> + Send>,
}

/// Contract: what applying gave.
#[derive(Clone, Debug, Default)]
pub struct ApplyOutcome {
    /// The new version (unchanged when nothing was applied).
    pub version: i64,
    /// docs/API.md `Change` objects.
    pub changes: Vec<Value>,
    /// docs/API.md `Overwrite` objects.
    pub overwrote: Vec<Value>,
    pub skipped: usize,
    /// The tabs whose contents changed (for keep-updated code tabs).
    pub tabs_changed: Vec<calc_engine::TabId>,
}

/// Contract: what viewers of a line are told (each becomes one server-sent event: `event: <name>`).
#[derive(Clone, Debug)]
pub enum LineEvent {
    Change(Value),
    Presence(Value),
    Comment(Value),
    Copy(Value),
    /// Too much changed to describe: reload everything.
    Reset,
}

/// Contract: a handle on one open line.
#[derive(Clone)]
pub struct LineHandle {
    pub line_id: Arc<str>,
    pub sheet_id: Arc<str>,
}

impl LineHandle {
    /// Runs `f` on the line's current workbook and version.
    pub async fn read<R: Send + 'static>(&self, _f: impl FnOnce(&Workbook, i64) -> R + Send + 'static) -> ApiResult<R> {
        Err(not_built(&self.line_id))
    }

    /// Applies one change as one version, saves it, recalculates and tells viewers.
    pub async fn apply(&self, _req: ApplyRequest) -> ApiResult<ApplyOutcome> {
        Err(not_built(&self.line_id))
    }

    /// Tells everyone watching this line.
    pub fn publish(&self, _event: LineEvent) {}
}

/// Contract: a line row.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct LineRow {
    pub id: String,
    pub sheet_id: String,
    pub is_main: bool,
    pub name: Option<String>,
    pub parent_id: Option<String>,
    pub forked_at: Option<i64>,
    pub head: i64,
    pub status: String,
}

/// Contract: the line `{sheet}` (main) or `{sheet}/copies/{copy}` names, after checking the copy belongs to the
/// sheet. Deleted copies are returned too (they are read-only; editing them answers 409).
pub async fn line_for(state: &AppState, sheet_id: &str, copy_id: Option<&str>) -> ApiResult<LineRow> {
    let id = match copy_id {
        Some(c) => c.to_owned(),
        None => crate::ids::main_line(sheet_id),
    };
    let row: Option<LineRow> = sqlx::query_as(
        "select id, sheet_id, is_main, name, parent_id, forked_at, head, status from lines where id = $1 and sheet_id = $2",
    )
    .bind(&id)
    .bind(sheet_id)
    .fetch_optional(&state.db)
    .await?;
    row.ok_or_else(|| crate::error::ApiError::not_found(if copy_id.is_some() { "This copy" } else { "This sheet" }))
}

/// Contract: the workbook of a line as it was at `version` (nearest snapshot, then the operations after it).
pub async fn workbook_at(_state: &AppState, line_id: &str, _version: i64) -> ApiResult<Workbook> {
    Err(not_built(line_id))
}

pub fn router() -> Router<AppState> {
    Router::new()
}

/// Unloads idle lines, writes due snapshots, reruns keep-updated code tabs.
pub fn spawn_maintenance(_state: AppState) {}

/// Contract: creates the main line of a new sheet holding `wb` (version 0), inside the caller's transaction.
pub async fn create_main_line(
    state: &AppState,
    conn: &mut sqlx::PgConnection,
    sheet_id: &str,
    author: Uuid,
    wb: &Workbook,
) -> ApiResult<()> {
    let line = crate::ids::main_line(sheet_id);
    let bytes = snapshot::encode(wb);
    let key = state.blobs.put(&bytes).await?;
    sqlx::query("insert into lines (id, sheet_id, is_main, head, created_by) values ($1, $2, true, 0, $3)")
        .bind(&line)
        .bind(sheet_id)
        .bind(author)
        .execute(&mut *conn)
        .await?;
    sqlx::query("insert into snapshots (line_id, seq, blob, bytes) values ($1, 0, $2, $3)")
        .bind(&line)
        .bind(&key)
        .bind(bytes.len() as i64)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Contract: docs/API.md `Tab` JSON for one tab.
pub fn tab_json(_wb: &Workbook, tab: &calc_engine::Tab, index: usize) -> Value {
    tab_json_basic(tab, index)
}

/// Contract: the main line's version and tabs (docs/API.md `Tab`) for `GET /v1/sheets/{id}`.
pub async fn sheet_tabs(state: &AppState, sheet_id: &str) -> ApiResult<(i64, Vec<Value>)> {
    let line = crate::ids::main_line(sheet_id);
    let row: Option<(i64, String)> =
        sqlx::query_as("select seq, blob from snapshots where line_id = $1 order by seq desc limit 1")
            .bind(&line)
            .fetch_optional(&state.db)
            .await?;
    let Some((seq, blob)) = row else { return Ok((0, Vec::new())) };
    let wb = snapshot::decode(&state.blobs.get(&blob).await?).map_err(crate::error::ApiError::internal)?;
    Ok((seq, wb.tabs.iter().enumerate().map(|(i, t)| tab_json_basic(t, i)).collect()))
}

/// Contract: the number of open copies of a sheet.
pub async fn open_copies(state: &AppState, sheet_id: &str) -> ApiResult<i64> {
    let n: (i64,) = sqlx::query_as("select count(*) from lines where sheet_id = $1 and not is_main and status = 'open'")
        .bind(sheet_id)
        .fetch_one(&state.db)
        .await?;
    Ok(n.0)
}

/// Contract: the main line's latest version (head), for sheet lists.
pub async fn head_version(state: &AppState, sheet_id: &str) -> ApiResult<i64> {
    let v: Option<(i64,)> = sqlx::query_as("select head from lines where id = $1")
        .bind(crate::ids::main_line(sheet_id))
        .fetch_optional(&state.db)
        .await?;
    Ok(v.map(|v| v.0).unwrap_or(0))
}

fn tab_json_basic(t: &calc_engine::Tab, index: usize) -> Value {
    let used = t.used_rect();
    json!({
        "id": t.id, "name": t.name, "index": index,
        "rows": used.map_or(0, |r| r.r2 + 1).max(1000), "cols": used.map_or(0, |r| r.c2 + 1).max(26),
        "used": used.map(|r| r.to_string()), "frozen_rows": t.frozen_rows, "frozen_cols": t.frozen_cols,
        "color": t.color, "code": Value::Null,
    })
}
