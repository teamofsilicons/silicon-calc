//! Sheets: listing, creating, renaming, moving, the trash, deleting forever, `GET /v1/me`, `/v1/meta`,
//! account lookups. See docs/API.md.

use axum::Router;

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
}
