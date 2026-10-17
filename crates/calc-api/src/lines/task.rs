//! One open line's task. It owns every change to the line's workbook: changes arrive as commands and are applied,
//! saved (the operation, its inverse and the new head in one transaction) and announced one at a time. Readers go
//! straight to the shared state and never wait for this task.
//!
//! Snapshots are made from a copy of the workbook taken under the read lock and encoded on a blocking thread in
//! the background, so a big line keeps taking changes while its snapshot is written.

use std::sync::{Arc, atomic::Ordering};

use calc_engine::{Op, Workbook};
use futures::future::BoxFuture;
use serde_json::{Value, json};
use time::OffsetDateTime;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use super::{
    ApplyOutcome, ApplyRequest, LineEvent, LineHandle, OpKind,
    apply::{self, Ctx, Done, Mode},
    events,
    registry::{LineState, Shared, Slot},
    store, view,
};
use crate::{
    error::{ApiError, ApiResult},
    state::AppState,
};

/// One step of a change: builds an operation from the workbook as the earlier steps left it.
pub(crate) type Stage = Box<dyn FnOnce(&Workbook) -> ApiResult<Option<Op>> + Send>;

/// Runs inside the transaction that saves a change, with its version (notifications, copy bookkeeping).
pub(crate) type TxHook = Box<dyn for<'c> FnOnce(&'c mut sqlx::PgConnection, i64) -> BoxFuture<'c, ApiResult<()>> + Send>;

/// A change to apply as one version.
pub(crate) struct Job {
    pub author: Uuid,
    pub kind: OpKind,
    pub summary: Option<String>,
    pub base_version: Option<i64>,
    pub client_id: Option<String>,
    pub stages: Vec<Stage>,
    /// The number of edits in the request this came from (errors then name the failing one), or 0.
    pub edits: usize,
    pub undoes: Option<i64>,
    pub redoes: Option<i64>,
    pub hook: Option<TxHook>,
    /// Apply only if the line is still at this version (the change was worked out from it); 409 otherwise.
    pub expect_version: Option<i64>,
}

impl Job {
    pub fn new(author: Uuid, kind: OpKind, stages: Vec<Stage>) -> Self {
        Self {
            author,
            kind,
            summary: None,
            base_version: None,
            client_id: None,
            stages,
            edits: 0,
            undoes: None,
            redoes: None,
            hook: None,
            expect_version: None,
        }
    }

    pub fn from_request(req: ApplyRequest) -> Self {
        let mut job = Self::new(req.author, req.kind, vec![req.build]);
        job.summary = req.summary;
        job.base_version = req.base_version;
        job.client_id = req.client_id;
        job
    }
}

pub(crate) enum Cmd {
    Job(Box<Job>, oneshot::Sender<ApiResult<ApplyOutcome>>),
    Undo {
        author: Uuid,
        redo: bool,
        base_version: Option<i64>,
        client_id: Option<String>,
        reply: oneshot::Sender<ApiResult<ApplyOutcome>>,
    },
    Snapshot(oneshot::Sender<ApiResult<i64>>),
    Tick,
    Unload(oneshot::Sender<bool>),
    Stop { save: bool, reply: Option<oneshot::Sender<()>> },
}

struct Line {
    app: AppState,
    shared: Arc<Shared>,
    /// The version of the latest snapshot written.
    last_snapshot: i64,
    /// A snapshot being written in the background, and its version.
    writing: Option<(i64, tokio::task::JoinHandle<ApiResult<i64>>)>,
}

/// Loads a line and starts its task.
pub(crate) async fn start(app: &AppState, line_id: &str) -> ApiResult<LineHandle> {
    let row: Option<(String, bool)> = sqlx::query_as("select sheet_id, is_main from lines where id = $1")
        .bind(line_id)
        .fetch_optional(&app.db)
        .await?;
    let Some((sheet_id, is_main)) = row else {
        return Err(ApiError::not_found(if line_id.starts_with("main:") { "This sheet" } else { "This copy" }));
    };
    let loaded = store::load(app, line_id).await?;
    let (tx, rx) = mpsc::unbounded_channel();
    let shared = Arc::new(Shared::new(
        line_id,
        &sheet_id,
        is_main,
        LineState::new(loaded.wb, loaded.version),
        tx,
        app.lines.inner.epoch,
    ));
    let handle = LineHandle { line_id: shared.line_id.clone(), sheet_id: shared.sheet_id.clone(), shared: shared.clone() };
    let line = Line { app: app.clone(), shared, last_snapshot: loaded.snapshot, writing: None };
    tokio::spawn(line.run(rx));
    Ok(handle)
}

/// Runs CPU work on a blocking thread.
pub(crate) async fn blocking<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> ApiResult<R> {
    tokio::task::spawn_blocking(f).await.map_err(|e| ApiError::internal(format!("a line's work failed: {e}")))
}

/// Drops a big value off the async threads.
fn drop_later<T: Send + 'static>(v: T) {
    tokio::task::spawn_blocking(move || drop(v));
}

impl Line {
    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Cmd>) {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                Cmd::Job(job, reply) => {
                    let res = self.job(*job).await;
                    let _ = reply.send(res);
                }
                Cmd::Undo { author, redo, base_version, client_id, reply } => {
                    let res = self.undo(author, redo, base_version, client_id).await;
                    let _ = reply.send(res);
                }
                Cmd::Snapshot(reply) => {
                    let res = self.snapshot_now().await;
                    let _ = reply.send(res);
                }
                Cmd::Tick => {
                    if self.shared.prune_presence() {
                        let ev = events::presence_event(&self.app, &self.shared).await;
                        self.shared.publish(LineEvent::Presence(ev));
                    }
                    if self.shared.idle_for() >= self.app.cfg.line_idle && self.try_unload().await {
                        break;
                    }
                    self.snapshot_if_due().await;
                }
                Cmd::Unload(reply) => {
                    let done = self.try_unload().await;
                    let _ = reply.send(done);
                    if done {
                        break;
                    }
                }
                Cmd::Stop { save, reply } => {
                    if save {
                        self.save().await;
                    } else if let Some((_, h)) = self.writing.take() {
                        let _ = h.await;
                    }
                    if let Some(r) = reply {
                        let _ = r.send(());
                    }
                    break;
                }
            }
        }
        // Live streams end now; viewers reconnect to the line as it opens again.
        let _ = self.shared.closed.send(true);
        tracing::debug!(line = %self.shared.line_id, "line closed");
    }

    /// Leaves the registry when nothing holds the line, saving a snapshot first.
    async fn try_unload(&mut self) -> bool {
        {
            let mut slots = self.app.lines.inner.slots.lock().expect("slots lock");
            // The registry's handle and this task are the only holders when nobody is using the line.
            if Arc::strong_count(&self.shared) > 2 || self.shared.events.receiver_count() > 0 {
                return false;
            }
            match slots.get(&*self.shared.line_id) {
                Some(Slot::Open(h)) if Arc::ptr_eq(&h.shared, &self.shared) => {
                    slots.remove(&*self.shared.line_id);
                }
                _ => return false,
            }
        }
        self.save().await;
        true
    }

    async fn owner(&self) -> ApiResult<Uuid> {
        let row: Option<(Uuid,)> = sqlx::query_as("select owner from sheets where id = $1")
            .bind(&*self.shared.sheet_id)
            .fetch_optional(&self.app.db)
            .await?;
        row.map(|r| r.0).ok_or_else(|| ApiError::not_found("This sheet"))
    }

    fn handle(&self) -> LineHandle {
        LineHandle { line_id: self.shared.line_id.clone(), sheet_id: self.shared.sheet_id.clone(), shared: self.shared.clone() }
    }

    async fn job(&mut self, job: Job) -> ApiResult<ApplyOutcome> {
        let Job { author, kind, summary, base_version, client_id, stages, edits, undoes, redoes, hook, expect_version } = job;
        let before = self.shared.version();
        if let Some(v) = expect_version
            && v != before
        {
            return Err(ApiError::conflict(
                format!("The sheet changed while this was being worked out (it was at version {v}, now {before})."),
                "Try again; it is worked out afresh from the latest version.",
            ));
        }
        let owner = self.owner().await?;
        let at = now();
        let ctx = Ctx {
            author: author.to_string(),
            owner: owner.to_string(),
            at: at.unix_timestamp(),
            index_errors: edits > 0,
            kind,
            summary,
        };
        let Some(done) = self.apply(stages, ctx).await? else {
            return Ok(ApplyOutcome { version: before, ..Default::default() });
        };
        let Done { op, inverse, staged, info } = done;
        let seq = before + 1;
        let meta = store::OpMeta { seq, author, at, kind, client_id: client_id.clone(), undoes, redoes, staged };
        if let Err(e) = store::persist(&self.app, &self.shared, &meta, op, inverse, &info, hook).await {
            tracing::error!(line = %self.shared.line_id, error = %e.message, "saving a change failed; rebuilding the line");
            self.reload().await;
            return Err(e);
        }
        self.shared.version.store(seq, Ordering::SeqCst);
        self.app.notify.wake();

        // Cells someone else changed after the version the caller was looking at.
        let overwrites = match base_version {
            Some(b) if b < before && !info.candidates.is_empty() => {
                match store::overwrites(&self.app, &self.shared.line_id, b, before, author, &info.candidates).await {
                    Ok(o) => o,
                    Err(e) => {
                        tracing::warn!(error = %e.message, "finding overwritten cells failed");
                        Vec::new()
                    }
                }
            }
            _ => Vec::new(),
        };
        let mut uuids: Vec<Uuid> = vec![author];
        uuids.extend(overwrites.iter().map(|o| o.by));
        if let Some(ec) = &info.event_cells {
            uuids.extend(ec.uuids.iter().filter_map(|u| Uuid::parse_str(u).ok()));
        }
        let accounts = self.app.accounts.get_map(uuids).await;
        let account = |u: &Uuid| view::account_json(accounts.get(u), *u);
        let overwrote: Vec<Value> = overwrites
            .iter()
            .map(|o| {
                json!({"tab": o.tab, "a": o.address, "by": account(&o.by), "at": crate::db::rfc3339(o.at),
                       "before": o.before, "version": o.version})
            })
            .collect();

        let mut event = json!({
            "version": seq, "by": account(&author), "at": crate::db::rfc3339(at),
            "kind": kind.as_str(), "summary": info.summary, "changes": info.changes,
        });
        if let Some(c) = &client_id {
            event["client_id"] = json!(c);
        }
        if !overwrote.is_empty() {
            event["overwrote"] = json!(overwrote);
        }
        if let Some(ec) = &info.event_cells {
            event["cells"] = json!(ec.cells);
            event["styles"] = ec.styles.clone();
            let acc: serde_json::Map<String, Value> = ec
                .uuids
                .iter()
                .filter_map(|u| Uuid::parse_str(u).ok())
                .map(|u| (u.to_string(), account(&u)))
                .collect();
            event["accounts"] = Value::Object(acc);
        }
        self.shared.publish(LineEvent::Change(event));

        if !info.tabs_changed.is_empty() {
            crate::code::rerun_dependents(&self.app, &self.handle(), &info.tabs_changed);
        }
        self.snapshot_if_due().await;
        Ok(ApplyOutcome { version: seq, changes: info.changes, overwrote, skipped: info.skipped, tabs_changed: info.tabs_changed })
    }

    /// Applies a change in memory (atomically): see `apply`.
    async fn apply(&mut self, mut stages: Vec<Stage>, ctx: Ctx) -> ApiResult<Option<Done>> {
        if stages.len() == 1 {
            let stage = stages.pop().expect("one stage");
            let guard = self.shared.state.clone().read_owned().await;
            let c = ctx.clone();
            let planned = blocking(move || -> ApiResult<Option<(Op, Option<Workbook>)>> {
                Ok(match apply::plan(&guard.wb, stage, &c)? {
                    None => None,
                    Some((op, Mode::Light)) => Some((op, None)),
                    Some((op, Mode::Heavy)) => Some((op, Some(guard.wb.clone()))),
                })
            })
            .await??;
            let Some((op, copy)) = planned else { return Ok(None) };
            match copy {
                None => {
                    let guard = self.shared.state.clone().write_owned().await;
                    let res = blocking(move || {
                        let mut g = guard;
                        apply::in_place(&mut g, op, &ctx)
                    })
                    .await;
                    match res {
                        Ok(Ok(done)) => Ok(Some(done)),
                        Ok(Err(fail)) => {
                            if fail.mutated {
                                self.reload().await;
                            }
                            Err(fail.error)
                        }
                        Err(e) => {
                            self.reload().await;
                            Err(e)
                        }
                    }
                }
                Some(mut wb) => {
                    let res = blocking(move || apply::on_copy(&mut wb, op, &ctx).map(|d| (wb, d))).await??;
                    Ok(Some(self.swap(res).await))
                }
            }
        } else {
            let guard = self.shared.state.clone().read_owned().await;
            let res = blocking(move || -> ApiResult<Option<(Workbook, Done)>> {
                let mut wb = guard.wb.clone();
                drop(guard);
                Ok(apply::staged(&mut wb, stages, &ctx)?.map(|d| (wb, d)))
            })
            .await??;
            match res {
                None => Ok(None),
                Some(r) => Ok(Some(self.swap(r).await)),
            }
        }
    }

    /// Puts a workbook changed on the side in place of the line's.
    async fn swap(&mut self, (wb, done): (Workbook, Done)) -> Done {
        let old = {
            let mut g = self.shared.state.write().await;
            let old = std::mem::replace(&mut g.wb, wb);
            g.version += 1;
            // Anything may have moved: used areas are worked out again when next asked.
            g.used.lock().expect("used lock").clear();
            old
        };
        drop_later(old);
        done
    }

    /// Rebuilds the line from what is saved (after a change failed half way, or failed to save).
    async fn reload(&mut self) {
        match store::load(&self.app, &self.shared.line_id).await {
            Ok(loaded) => {
                let old = {
                    let mut g = self.shared.state.write().await;
                    std::mem::replace(&mut *g, LineState::new(loaded.wb, loaded.version))
                };
                drop_later(old);
                self.shared.version.store(loaded.version, Ordering::SeqCst);
                self.last_snapshot = self.last_snapshot.max(loaded.snapshot);
                self.shared.publish(LineEvent::Reset);
            }
            Err(e) => tracing::error!(line = %self.shared.line_id, error = %e.message, "rebuilding a line failed"),
        }
    }

    async fn undo(&mut self, author: Uuid, redo: bool, base_version: Option<i64>, client_id: Option<String>) -> ApiResult<ApplyOutcome> {
        let Some(target) = store::undo_target(&self.app, &self.shared.line_id, author, redo).await? else {
            return Err(if redo {
                ApiError::conflict(
                    "There is nothing of yours to redo on this sheet.",
                    "Redo brings back a change you undid, until you make a new change.",
                )
            } else {
                ApiError::conflict(
                    "There is nothing of yours to undo on this sheet.",
                    "Undo only takes back your own changes; to go back further or undo someone else's, restore a version.",
                )
            });
        };
        let inverse = target.inverse;
        let summary = format!("{} \"{}\"", if redo { "redid" } else { "undid" }, target.summary);
        let mut job = Job::new(author, if redo { OpKind::Redo } else { OpKind::Undo }, vec![Box::new(move |_| Ok(Some(inverse)))]);
        job.summary = Some(summary);
        job.base_version = base_version;
        job.client_id = client_id;
        if redo {
            job.redoes = Some(target.seq);
        } else {
            job.undoes = Some(target.seq);
        }
        self.job(job).await
    }

    /// Notes a background snapshot that finished.
    async fn collect(&mut self, wait: bool) {
        let finished = match &self.writing {
            Some((_, h)) => wait || h.is_finished(),
            None => false,
        };
        if !finished {
            return;
        }
        let (v, h) = self.writing.take().expect("checked");
        match h.await {
            Ok(Ok(written)) => self.last_snapshot = self.last_snapshot.max(written),
            Ok(Err(e)) => tracing::warn!(line = %self.shared.line_id, version = v, error = %e.message, "writing a snapshot failed"),
            Err(e) => tracing::warn!(line = %self.shared.line_id, version = v, error = %e, "writing a snapshot failed"),
        }
    }

    /// Starts writing a snapshot of the line as it is now, in the background.
    async fn start_snapshot(&mut self) -> ApiResult<()> {
        let guard = self.shared.state.clone().read_owned().await;
        let (wb, version) = blocking(move || (guard.wb.clone(), guard.version)).await?;
        let (app, line, sheet, main) = (self.app.clone(), self.shared.line_id.to_string(), self.shared.sheet_id.to_string(), self.shared.is_main);
        let h = tokio::spawn(async move { store::write_snapshot(&app, &line, &sheet, main, wb, version).await });
        self.writing = Some((version, h));
        Ok(())
    }

    /// Writes a snapshot now (when the line moved on since the last one) and waits for it.
    async fn snapshot_now(&mut self) -> ApiResult<i64> {
        self.collect(true).await;
        if self.shared.version() > self.last_snapshot {
            self.start_snapshot().await?;
            let (_, h) = self.writing.take().expect("just started");
            let v = h.await.map_err(|e| ApiError::internal(format!("writing a snapshot failed: {e}")))??;
            self.last_snapshot = self.last_snapshot.max(v);
        }
        Ok(self.last_snapshot)
    }

    async fn snapshot_if_due(&mut self) {
        self.collect(false).await;
        if self.writing.is_some() {
            return;
        }
        if self.shared.version() - self.last_snapshot >= self.app.cfg.snapshot_every.max(1) as i64
            && let Err(e) = self.start_snapshot().await
        {
            tracing::warn!(line = %self.shared.line_id, error = %e.message, "starting a snapshot failed");
        }
    }

    /// Saves everything before the line closes.
    async fn save(&mut self) {
        if let Err(e) = self.snapshot_now().await {
            tracing::warn!(line = %self.shared.line_id, error = %e.message, "writing a snapshot failed");
        }
    }
}

/// Now, to the microsecond (what Postgres keeps), so the time saved is the time used.
pub(crate) fn now() -> OffsetDateTime {
    let t = OffsetDateTime::now_utc();
    t.replace_nanosecond(t.nanosecond() / 1_000 * 1_000).unwrap_or(t)
}
