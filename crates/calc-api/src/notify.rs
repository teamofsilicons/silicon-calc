//! Notifications: rows in `outbox`, written in the same transaction as their cause, sent to Silicon Ting by one
//! worker with Calc's App verification proof. Without `CALC_TING_URL` they are kept and marked `not_configured`.

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::Notify;
use uuid::Uuid;

use crate::{error::ApiResult, state::AppState};

#[derive(Clone, Default)]
pub struct Notifier {
    wake: Arc<Notify>,
}

impl Notifier {
    pub fn new() -> Self {
        Self::default()
    }

    /// Tells the worker there is something to send (after the transaction that wrote it committed).
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    pub async fn woken(&self) {
        self.wake.notified().await;
    }
}

/// Queues one ting. `key` makes it idempotent: the same key is never sent twice.
/// `data` is the ting's `data` (see docs/API.md, Notifications).
pub async fn enqueue(
    conn: &mut sqlx::PgConnection,
    kind: &str,
    recipient: Uuid,
    key: &str,
    data: Value,
) -> ApiResult<()> {
    let body = serde_json::json!({"type": kind, "for": recipient.to_string(), "key": key, "data": data, "metadata": {}});
    sqlx::query("insert into outbox (type, recipient, key, body) values ($1, $2, $3, $4) on conflict (key) do nothing")
        .bind(kind)
        .bind(recipient)
        .bind(key)
        .bind(body)
        .execute(conn)
        .await?;
    Ok(())
}

/// Starts the sender. Implemented by the lines work item with calc-ting.
pub fn spawn_worker(_state: AppState) {}
