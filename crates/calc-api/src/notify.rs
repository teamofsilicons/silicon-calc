//! Notifications: rows in `outbox`, written in the same transaction as their cause, sent to Silicon Ting by one
//! worker with Calc's App verification proof. Without `CALC_TING_URL` they are kept and marked `not_configured`.
//!
//! The worker takes due rows one at a time, oldest first, with `for update skip locked`, so several service
//! instances never send the same row. A row Ting accepted is marked `sent` with the ting's id. A failure that may
//! pass is retried later (after 1 second, doubling to 5 minutes, with jitter); one that can't is marked `failed`
//! with the reason. Ting de-duplicates on each ting's `key`, so a row sent again after a crash is shown once.

use std::{sync::Arc, time::Duration};

use futures::future::BoxFuture;
use rand::Rng as _;
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

/// What hands one ting to Silicon Ting: `calc_ting::Publisher` in the service, a stand-in in tests.
pub trait TingSender: Send + Sync {
    fn send<'a>(&'a self, body: &'a Value) -> BoxFuture<'a, Result<calc_ting::Accepted, calc_ting::Error>>;
}

impl TingSender for calc_ting::Publisher {
    fn send<'a>(&'a self, body: &'a Value) -> BoxFuture<'a, Result<calc_ting::Accepted, calc_ting::Error>> {
        Box::pin(calc_ting::Publisher::send(self, body))
    }
}

/// How often the worker looks for due rows when nothing wakes it.
pub const POLL: Duration = Duration::from_secs(3);
/// The longest wait before trying a row again.
pub const MAX_BACKOFF: Duration = Duration::from_secs(300);
/// What `not_configured` rows say.
pub const NOT_CONFIGURED: &str = "CALC_TING_URL is not set, so Silicon Calc keeps this ting without sending it.";

/// The wait before the next try of a row that failed `attempts` times: 1 s, 2 s, 4 s, … up to 5 minutes, each
/// within ±20% so many rows that failed together don't come back together.
pub fn backoff(attempts: i32) -> Duration {
    let base = 2f64.powi((attempts - 1).clamp(0, 30)).min(MAX_BACKOFF.as_secs_f64());
    let jitter = rand::rng().random_range(0.8..1.2);
    Duration::from_secs_f64((base * jitter).min(MAX_BACKOFF.as_secs_f64()))
}

/// What one pass over the due rows did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Round {
    pub sent: usize,
    /// Failed for now; tried again later.
    pub retried: usize,
    /// Failed for good.
    pub failed: usize,
    /// Ting (or Silicon Accounts, for the proof) couldn't be reached: the pass stopped there.
    pub unreachable: Option<String>,
}

impl Round {
    pub fn handled(&self) -> usize {
        self.sent + self.retried + self.failed
    }
}

fn clip(text: &str) -> String {
    text.chars().take(1000).collect()
}

/// Sends due rows, oldest first, until none are due, `limit` were handled, or Ting can't be reached.
pub async fn send_due(state: &AppState, sender: &dyn TingSender, limit: usize) -> ApiResult<Round> {
    let mut round = Round::default();
    while round.handled() < limit {
        let mut tx = state.db.begin().await?;
        let row: Option<(i64, Value, i32)> = sqlx::query_as(
            "select id, body, attempts from outbox
             where status = 'pending' and next_attempt_at <= now()
             order by id limit 1 for update skip locked",
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some((id, body, attempts)) = row else { break };
        let attempts = attempts.saturating_add(1);
        match sender.send(&body).await {
            Ok(accepted) => {
                sqlx::query(
                    "update outbox set status = 'sent', ting_id = $2, sent_at = now(), attempts = $3, last_error = null
                     where id = $1",
                )
                .bind(id)
                .bind(&accepted.id)
                .bind(attempts)
                .execute(&mut *tx)
                .await?;
                round.sent += 1;
            }
            Err(e) if e.retryable() => {
                sqlx::query(
                    "update outbox set attempts = $2, last_error = $3, next_attempt_at = now() + make_interval(secs => $4)
                     where id = $1",
                )
                .bind(id)
                .bind(attempts)
                .bind(clip(&e.to_string()))
                .bind(backoff(attempts).as_secs_f64())
                .execute(&mut *tx)
                .await?;
                round.retried += 1;
                if matches!(e, calc_ting::Error::Transport(_) | calc_ting::Error::Authority(_)) {
                    tx.commit().await?;
                    round.unreachable = Some(e.to_string());
                    break;
                }
            }
            Err(e) => {
                sqlx::query("update outbox set status = 'failed', attempts = $2, last_error = $3 where id = $1")
                    .bind(id)
                    .bind(attempts)
                    .bind(clip(&e.to_string()))
                    .execute(&mut *tx)
                    .await?;
                round.failed += 1;
            }
        }
        tx.commit().await?;
    }
    Ok(round)
}

/// Marks every waiting row `not_configured` (each once; rows are never deleted). For running without Ting.
pub async fn mark_not_configured(state: &AppState) -> ApiResult<u64> {
    let res = sqlx::query("update outbox set status = 'not_configured', last_error = $1 where status = 'pending'")
        .bind(NOT_CONFIGURED)
        .execute(&state.db)
        .await?;
    Ok(res.rows_affected())
}

/// How long until the next row is due (None: nothing is waiting).
async fn next_due_in(state: &AppState) -> ApiResult<Option<Duration>> {
    let secs: Option<(Option<f64>,)> = sqlx::query_as(
        "select extract(epoch from (min(next_attempt_at) - now()))::float8 from outbox where status = 'pending'",
    )
    .fetch_optional(&state.db)
    .await?;
    Ok(secs.and_then(|s| s.0).map(|s| Duration::from_secs_f64(s.max(0.0))))
}

/// Waits until woken, until the next row is due, or for `POLL`, whichever comes first.
async fn idle(state: &AppState) {
    let wait = match next_due_in(state).await {
        Ok(Some(d)) => d.min(POLL),
        _ => POLL,
    };
    if wait.is_zero() {
        return;
    }
    tokio::select! {
        _ = state.notify.woken() => {}
        _ = tokio::time::sleep(wait) => {}
    }
}

/// Runs the sender with `sender` until the process ends.
pub async fn run(state: AppState, sender: Arc<dyn TingSender>) {
    let mut outage: i32 = 0;
    loop {
        match send_due(&state, sender.as_ref(), 100).await {
            Ok(round) => {
                if let Some(why) = &round.unreachable {
                    outage = outage.saturating_add(1);
                    let wait = backoff(outage);
                    tracing::warn!(error = %why, retry_in = ?wait, "Silicon Ting can't be reached; tings wait in the outbox");
                    tokio::time::sleep(wait).await;
                    continue;
                }
                outage = 0;
                if round.handled() >= 100 {
                    continue;
                }
                idle(&state).await;
            }
            Err(e) => {
                tracing::warn!(error = %e.message, "sending tings failed; trying again shortly");
                tokio::time::sleep(POLL).await;
            }
        }
    }
}

/// Starts the sender: with `CALC_TING_URL`, `calc_ting::Publisher`; without it, a task that marks rows
/// `not_configured` as they come.
pub fn spawn_worker(state: AppState) {
    let Some(ting_url) = state.cfg.ting_url.clone() else {
        tokio::spawn(async move {
            loop {
                match mark_not_configured(&state).await {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(count = n, "CALC_TING_URL is not set: kept tings without sending them"),
                    Err(e) => tracing::warn!(error = %e.message, "marking tings as not sent failed"),
                }
                tokio::select! {
                    _ = state.notify.woken() => {}
                    _ = tokio::time::sleep(POLL) => {}
                }
            }
        });
        return;
    };
    tokio::spawn(async move {
        let cfg = calc_ting::Config {
            ting_url,
            accounts_api_url: state.cfg.accounts_api_url.clone(),
            app_id: state.cfg.app_id.clone(),
            app_secret: state.cfg.app_secret.clone(),
            ting_app_id: state.cfg.ting_app_id.clone(),
            allow_http: state.cfg.allow_http,
        };
        let publisher = loop {
            match calc_ting::Publisher::new(cfg.clone()) {
                Ok(p) => break p,
                Err(e) => {
                    tracing::error!(error = %e, "can't start sending tings; check CALC_TING_URL (trying again in a minute)");
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
            }
        };
        run(state, Arc::new(publisher)).await;
    });
}

/// Starts the sender with a given `TingSender` (tests, or another way to reach Ting).
pub fn spawn_worker_with(state: AppState, sender: Arc<dyn TingSender>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run(state, sender))
}
