//! Code: `POST {line}/run`, reruns of code-made tabs, `PATCH {line}/tabs/{tab}/code`. See docs/API.md.

use axum::Router;

use crate::{lines::LineHandle, state::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
}

/// Contract: called by `lines` after a version changed `changed_tabs`, so keep-updated code tabs that read them
/// rerun (as the account that set them up, after a short pause so bursts of edits rerun once).
pub fn rerun_dependents(_state: &AppState, _line: &LineHandle, _changed_tabs: &[calc_engine::TabId]) {}
