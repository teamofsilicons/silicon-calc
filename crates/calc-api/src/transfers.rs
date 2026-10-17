//! See docs/API.md and docs/BUILDING.md for what this module serves.

use axum::Router;

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
}
