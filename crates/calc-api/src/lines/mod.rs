//! Lines: a sheet's main line and its copies. Each open line is one task owning its `Workbook`; it applies
//! operations in order, writes them to `ops`, snapshots every so often, recalculates and tells viewers.
//! Routes: tabs, cells, edits, events, presence, versions, compare, restore, copies. See docs/API.md.
//!
//! The functions marked "contract" are called by other modules; keep their signatures.
//!
//! How it fits together:
//! - `registry`: the open lines, one task each (`task`), loading (latest snapshot, then the operations after it),
//!   unloading idle lines, saving on shutdown.
//! - `apply`: applying one change inside a line's task, atomically, and describing what it changed.
//! - `store`: the operations log, snapshots and cell history in Postgres and blob storage.
//! - `edits`: the edit types of `POST {line}/edits`, turned into engine operations.
//! - `view`: the JSON answers (tabs, cells, charts, protections).
//! - `routes`, `events`, `versions`, `copies`: the HTTP routes.

mod apply;
mod copies;
mod edits;
mod events;
mod registry;
mod restore;
mod routes;
mod store;
mod task;
mod versions;
mod view;

use std::sync::Arc;

use axum::Router;
use calc_engine::{Workbook, snapshot};
use serde_json::Value;
use uuid::Uuid;

use crate::{error::ApiResult, state::AppState};

pub(crate) use registry::Shared;

/// The open lines.
#[derive(Clone, Default)]
pub struct Registry {
    pub(crate) inner: Arc<registry::Inner>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Contract: the handle of an open line, loading it (latest snapshot + operations after it) if needed.
    pub async fn open(&self, state: &AppState, line_id: &str) -> ApiResult<LineHandle> {
        registry::open(self, state, line_id).await
    }

    /// Saves every open line (on shutdown).
    pub async fn shutdown(&self, state: &AppState) {
        registry::shutdown(self, state).await
    }

    /// Unloads an open line now if nothing holds it (no viewers, no handles in use), saving a snapshot first.
    /// Answers whether it was unloaded.
    pub async fn unload(&self, line_id: &str) -> bool {
        registry::unload(self, line_id).await
    }

    /// Drops an open line without saving anything, as a restart would: the next open rebuilds it from its latest
    /// snapshot and the operations after it.
    pub async fn discard(&self, line_id: &str) {
        registry::discard(self, line_id).await
    }

    /// The ids of the lines open right now.
    pub fn open_lines(&self) -> Vec<String> {
        registry::open_lines(self)
    }
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

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "create" => Self::Create,
            "edit" => Self::Edit,
            "import" => Self::Import,
            "code" => Self::Code,
            "merge" => Self::Merge,
            "restore" => Self::Restore,
            "undo" => Self::Undo,
            "redo" => Self::Redo,
            _ => return None,
        })
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
    pub(crate) shared: Arc<registry::Shared>,
}

impl LineHandle {
    /// Runs `f` on the line's current workbook and version. Reads never wait for the line's task; they wait only
    /// while a change is being written into the workbook in memory.
    pub async fn read<R: Send + 'static>(&self, f: impl FnOnce(&Workbook, i64) -> R + Send + 'static) -> ApiResult<R> {
        self.shared.read(move |st| f(&st.wb, st.version)).await
    }

    /// Applies one change as one version, saves it, recalculates and tells viewers.
    pub async fn apply(&self, req: ApplyRequest) -> ApiResult<ApplyOutcome> {
        let job = task::Job::from_request(req);
        self.shared.run(job).await
    }

    /// Tells everyone watching this line.
    pub fn publish(&self, event: LineEvent) {
        self.shared.publish(event);
    }

    /// The line's latest version.
    pub fn version(&self) -> i64 {
        self.shared.version()
    }

    /// How many viewers are watching this line's events.
    pub fn viewers(&self) -> usize {
        self.shared.events.receiver_count()
    }
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
pub async fn workbook_at(state: &AppState, line_id: &str, version: i64) -> ApiResult<Workbook> {
    let wb = versions::state_at(state, line_id, version).await?;
    Ok(Arc::try_unwrap(wb).unwrap_or_else(|shared| (*shared).clone()))
}

pub fn router() -> Router<AppState> {
    Router::new().merge(routes::router()).merge(versions::router()).merge(copies::router())
}

/// Unloads idle lines, writes due snapshots, reruns keep-updated code tabs.
pub fn spawn_maintenance(state: AppState) {
    registry::spawn_maintenance(state);
}

/// Contract: creates the main line of a new sheet holding `wb` (version 0), inside the caller's transaction.
pub async fn create_main_line(
    state: &AppState,
    conn: &mut sqlx::PgConnection,
    sheet_id: &str,
    author: Uuid,
    wb: &Workbook,
) -> ApiResult<()> {
    let line = crate::ids::main_line(sheet_id);
    let (bytes, text) = store::blocking_ref(|| (snapshot::encode(wb), store::cell_text(wb)));
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
    // The sheet is searchable by its cells from the start (an opened file's data included).
    sqlx::query("update sheets set search_text = left(title || E'\\n' || $2, 220000) where id = $1")
        .bind(sheet_id)
        .bind(&text)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Contract: docs/API.md `Tab` JSON for one tab. `code.run_by` names the account by uuid only; answers fill it in
/// with `resolve_tab_accounts`.
pub fn tab_json(_wb: &Workbook, tab: &calc_engine::Tab, index: usize) -> Value {
    view::tab_json(tab, index, tab.used_rect())
}

/// Fills in the `code.run_by` accounts of `Tab` JSON made by `tab_json` (one tab, or an array of them).
pub async fn resolve_tab_accounts(state: &AppState, tabs: &mut Value) {
    view::resolve_tab_accounts(state, tabs).await
}

/// Contract: the main line's version and tabs (docs/API.md `Tab`) for `GET /v1/sheets/{id}`.
pub async fn sheet_tabs(state: &AppState, sheet_id: &str) -> ApiResult<(i64, Vec<Value>)> {
    let line = state.lines.open(state, &crate::ids::main_line(sheet_id)).await?;
    let (version, tabs) = line.shared.read(|st| (st.version, view::tabs_json(st))).await?;
    let mut tabs = Value::Array(tabs);
    view::resolve_tab_accounts(state, &mut tabs).await;
    let Value::Array(tabs) = tabs else { unreachable!("built as an array") };
    Ok((version, tabs))
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
