//! What every handler shares.

use std::sync::Arc;

use sqlx::PgPool;

use crate::{accounts::Accounts, blobs::Blobs, config::Config};

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub db: PgPool,
    pub accounts: Accounts,
    pub blobs: Blobs,
    /// The open lines (main sheets and copies), each owned by one task. See `lines`.
    pub lines: crate::lines::Registry,
    /// Wakes the notification sender when the outbox gets a row. See `notify`.
    pub notify: crate::notify::Notifier,
}
