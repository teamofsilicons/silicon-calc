//! The open lines: one task each, loaded on first use (by one loader even when many ask at once), unloaded when
//! idle with nobody watching, saved on shutdown.
//!
//! A line may only be unloaded when nothing outside the registry holds a handle on it: every `LineHandle` holds the
//! line's `Shared`, so the task counts the holders (the registry's own handle and the task itself make two) while
//! holding the registry's lock, which is the only place new handles come from.

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use calc_engine::{Rect, Tab, TabId, Workbook};
use serde_json::Value;
use time::OffsetDateTime;
use tokio::sync::{RwLock, broadcast, mpsc, oneshot, watch};
use uuid::Uuid;

use super::{ApplyOutcome, LineEvent, LineHandle, Registry, task};
use crate::{
    error::{ApiError, ApiResult},
    state::AppState,
};

/// How many recent change events each open line keeps for viewers who reconnect, and how many bytes of them.
const RECENT_EVENTS: usize = 1_000;
const RECENT_BYTES: usize = 16 << 20;
/// How much memory past states of lines (for reading and comparing versions) may take, roughly, and the biggest
/// one kept.
const STATES_BYTES: u64 = 256 << 20;
const STATE_MAX_BYTES: u64 = 96 << 20;

pub(crate) enum Slot {
    Loading(watch::Receiver<Option<Result<LineHandle, ApiError>>>),
    Open(LineHandle),
}

/// Past states of lines, by `(line, version)`, least recently used first out, bounded by their rough size.
#[derive(Default)]
pub(crate) struct StateCache {
    entries: VecDeque<((String, i64), Arc<Workbook>, u64)>,
    bytes: u64,
}

/// Roughly how much memory a workbook takes.
pub(crate) fn approx_bytes(wb: &Workbook) -> u64 {
    wb.tabs.iter().map(|t| t.rows.len() as u64 * t.cols.len().max(1) as u64 * 12 + 4096).sum::<u64>() + 4096
}

impl StateCache {
    pub(crate) fn get(&mut self, line: &str, version: i64) -> Option<Arc<Workbook>> {
        let i = self.entries.iter().position(|((l, v), _, _)| l == line && *v == version)?;
        let e = self.entries.remove(i)?;
        let wb = e.1.clone();
        self.entries.push_back(e);
        Some(wb)
    }

    /// The cached state of `line` nearest below or at `version` and at or above `floor`.
    pub(crate) fn nearest(&self, line: &str, version: i64, floor: i64) -> Option<(i64, Arc<Workbook>)> {
        self.entries
            .iter()
            .filter(|((l, v), _, _)| l == line && *v <= version && *v >= floor)
            .max_by_key(|((_, v), _, _)| *v)
            .map(|((_, v), wb, _)| (*v, wb.clone()))
    }

    pub(crate) fn put(&mut self, line: &str, version: i64, wb: Arc<Workbook>) {
        let size = approx_bytes(&wb);
        if size > STATE_MAX_BYTES || self.entries.iter().any(|((l, v), _, _)| l == line && *v == version) {
            return;
        }
        self.entries.push_back(((line.to_owned(), version), wb, size));
        self.bytes += size;
        while self.bytes > STATES_BYTES {
            match self.entries.pop_front() {
                Some((_, _, s)) => self.bytes = self.bytes.saturating_sub(s),
                None => break,
            }
        }
    }

    /// Forgets every state of a line (it was deleted, or rebuilt after a failure).
    pub(crate) fn forget(&mut self, line: &str) {
        self.entries.retain(|((l, _), _, _)| l != line);
        self.bytes = self.entries.iter().map(|(_, _, s)| *s).sum();
    }
}

pub(crate) struct Inner {
    pub(crate) slots: Mutex<HashMap<String, Slot>>,
    /// Past states of lines, for reading and comparing versions.
    pub(crate) states: Mutex<StateCache>,
    /// When the registry was made: the clock `Shared::touched` counts from.
    pub(crate) epoch: Instant,
}

impl Default for Inner {
    fn default() -> Self {
        Self { slots: Mutex::new(HashMap::new()), states: Mutex::new(StateCache::default()), epoch: Instant::now() }
    }
}

/// A line's workbook and version, as readers see them. Only the line's task changes it.
pub(crate) struct LineState {
    pub wb: Workbook,
    pub version: i64,
    /// Each tab's used area, worked out when first asked and kept up to date as cells change, so reading a window
    /// of a huge tab never scans the whole tab. A tab missing here is worked out again.
    pub used: Mutex<HashMap<TabId, Option<Rect>>>,
}

impl LineState {
    pub fn new(wb: Workbook, version: i64) -> Self {
        Self { wb, version, used: Mutex::new(HashMap::new()) }
    }

    /// The used area of a tab (cached).
    pub fn used(&self, tab: &Tab) -> Option<Rect> {
        if let Some(u) = self.used.lock().expect("used lock").get(&tab.id) {
            return *u;
        }
        let u = tab.used_rect();
        self.used.lock().expect("used lock").insert(tab.id, u);
        u
    }
}

/// Someone looking at a line.
#[derive(Clone, Debug)]
pub(crate) struct Presence {
    pub account: Uuid,
    pub tab: Option<u32>,
    pub range: Option<String>,
    pub client_id: String,
    pub seen_at: OffsetDateTime,
    pub seen: Instant,
}

/// How long presence lasts without being sent again.
pub(crate) const PRESENCE_TTL: Duration = Duration::from_secs(45);

/// What a line's task shares with its handles.
pub(crate) struct Shared {
    pub line_id: Arc<str>,
    pub sheet_id: Arc<str>,
    pub is_main: bool,
    pub(crate) cmds: mpsc::UnboundedSender<task::Cmd>,
    pub(crate) state: Arc<RwLock<LineState>>,
    pub(crate) events: broadcast::Sender<LineEvent>,
    /// Becomes true when the line's task has stopped (unloaded, dropped, shut down): live streams end then, and
    /// viewers reconnect to the line as it is loaded again.
    pub(crate) closed: watch::Sender<bool>,
    pub(crate) presence: Mutex<HashMap<String, Presence>>,
    /// Recent change events (version, event), oldest first, and their total size, for viewers who reconnect.
    pub(crate) recent: Mutex<(VecDeque<(i64, Arc<Value>, usize)>, usize)>,
    /// The latest saved version.
    pub(crate) version: AtomicI64,
    /// Milliseconds since the registry's epoch when the line was last used.
    pub(crate) touched: AtomicU64,
    epoch: Instant,
}

impl Shared {
    pub(crate) fn new(
        line_id: &str,
        sheet_id: &str,
        is_main: bool,
        state: LineState,
        cmds: mpsc::UnboundedSender<task::Cmd>,
        epoch: Instant,
    ) -> Self {
        let version = state.version;
        let (events, _) = broadcast::channel(4096);
        let (closed, _) = watch::channel(false);
        let s = Self {
            line_id: Arc::from(line_id),
            sheet_id: Arc::from(sheet_id),
            is_main,
            cmds,
            state: Arc::new(RwLock::new(state)),
            events,
            closed,
            presence: Mutex::new(HashMap::new()),
            recent: Mutex::new((VecDeque::new(), 0)),
            version: AtomicI64::new(version),
            touched: AtomicU64::new(0),
            epoch,
        };
        s.touch();
        s
    }

    pub(crate) fn version(&self) -> i64 {
        self.version.load(Ordering::SeqCst)
    }

    pub(crate) fn touch(&self) {
        self.touched.store(self.epoch.elapsed().as_millis() as u64, Ordering::Relaxed);
    }

    pub(crate) fn idle_for(&self) -> Duration {
        let now = self.epoch.elapsed().as_millis() as u64;
        Duration::from_millis(now.saturating_sub(self.touched.load(Ordering::Relaxed)))
    }

    /// Runs `f` on the line's state on a blocking thread, while no change is being written into it.
    pub(crate) async fn read<R: Send + 'static>(&self, f: impl FnOnce(&LineState) -> R + Send + 'static) -> ApiResult<R> {
        self.touch();
        let guard = self.state.clone().read_owned().await;
        tokio::task::spawn_blocking(move || f(&guard))
            .await
            .map_err(|e| ApiError::internal(format!("reading a line failed: {e}")))
    }

    /// Hands a change to the line's task and waits for it to be applied and saved.
    pub(crate) async fn run(&self, job: task::Job) -> ApiResult<ApplyOutcome> {
        self.touch();
        let (tx, rx) = oneshot::channel();
        self.cmds.send(task::Cmd::Job(Box::new(job), tx)).map_err(|_| closed())?;
        rx.await.map_err(|_| closed())?
    }

    /// Undoes (or redoes) the caller's own last change on this line.
    pub(crate) async fn undo(
        &self,
        author: Uuid,
        redo: bool,
        base_version: Option<i64>,
        client_id: Option<String>,
    ) -> ApiResult<ApplyOutcome> {
        self.touch();
        let (tx, rx) = oneshot::channel();
        self.cmds.send(task::Cmd::Undo { author, redo, base_version, client_id, reply: tx }).map_err(|_| closed())?;
        rx.await.map_err(|_| closed())?
    }

    /// Writes a snapshot of the line now and waits for it; answers its version.
    pub(crate) async fn snapshot(&self) -> ApiResult<i64> {
        let (tx, rx) = oneshot::channel();
        self.cmds.send(task::Cmd::Snapshot(tx)).map_err(|_| closed())?;
        rx.await.map_err(|_| closed())?
    }

    pub(crate) fn publish(&self, event: LineEvent) {
        if let LineEvent::Change(v) = &event
            && let Some(version) = v.get("version").and_then(Value::as_i64)
        {
            let size = v.to_string().len();
            let mut recent = self.recent.lock().expect("recent lock");
            recent.0.push_back((version, Arc::new(v.clone()), size));
            recent.1 += size;
            while recent.0.len() > RECENT_EVENTS || (recent.1 > RECENT_BYTES && recent.0.len() > 1) {
                if let Some((_, _, s)) = recent.0.pop_front() {
                    recent.1 = recent.1.saturating_sub(s);
                }
            }
        }
        let _ = self.events.send(event);
    }

    /// The change events in `(since, head]`, when every one of them is still kept in memory.
    pub(crate) fn recent_since(&self, since: i64, head: i64) -> Option<Vec<Arc<Value>>> {
        if since >= head {
            return Some(Vec::new());
        }
        let recent = self.recent.lock().expect("recent lock");
        let first = recent.0.front()?.0;
        if first > since + 1 {
            return None;
        }
        let out: Vec<Arc<Value>> =
            recent.0.iter().filter(|(v, _, _)| *v > since && *v <= head).map(|(_, e, _)| e.clone()).collect();
        // Every version must be there (a change that failed to save is never published).
        (out.len() as i64 == head - since).then_some(out)
    }

    /// Who is looking at the line now (expired entries dropped).
    pub(crate) fn viewers(&self) -> Vec<Presence> {
        let mut p = self.presence.lock().expect("presence lock");
        p.retain(|_, v| v.seen.elapsed() < PRESENCE_TTL);
        let mut list: Vec<Presence> = p.values().cloned().collect();
        list.sort_by(|a, b| a.seen_at.cmp(&b.seen_at).then_with(|| a.client_id.cmp(&b.client_id)));
        list
    }

    /// Drops expired presence; answers whether anything expired.
    pub(crate) fn prune_presence(&self) -> bool {
        let mut p = self.presence.lock().expect("presence lock");
        let before = p.len();
        p.retain(|_, v| v.seen.elapsed() < PRESENCE_TTL);
        p.len() != before
    }
}

fn closed() -> ApiError {
    ApiError::unavailable(
        "This sheet was just closed in Silicon Calc's memory while the request was on its way.",
        "Try again; it opens again by itself.",
    )
}

pub(crate) async fn open(reg: &Registry, state: &AppState, line_id: &str) -> ApiResult<LineHandle> {
    loop {
        let mut waiter = {
            let mut slots = reg.inner.slots.lock().expect("slots lock");
            match slots.get(line_id) {
                Some(Slot::Open(h)) => {
                    h.shared.touch();
                    return Ok(h.clone());
                }
                Some(Slot::Loading(rx)) => rx.clone(),
                None => {
                    let (tx, rx) = watch::channel(None);
                    slots.insert(line_id.to_owned(), Slot::Loading(rx.clone()));
                    let (app, id, reg) = (state.clone(), line_id.to_owned(), reg.clone());
                    // The load runs on its own, so a caller giving up never leaves the line half loaded.
                    tokio::spawn(async move {
                        let res = task::start(&app, &id).await;
                        {
                            let mut slots = reg.inner.slots.lock().expect("slots lock");
                            match &res {
                                Ok(h) => {
                                    slots.insert(id.clone(), Slot::Open(h.clone()));
                                }
                                Err(_) => {
                                    slots.remove(&id);
                                }
                            }
                        }
                        let _ = tx.send(Some(res));
                    });
                    rx
                }
            }
        };
        let outcome = match waiter.wait_for(Option::is_some).await {
            Ok(v) => v.as_ref().cloned(),
            Err(_) => None,
        };
        match outcome {
            Some(res) => return res,
            None => {
                // The loader went away without an answer: forget its slot and try again.
                let mut slots = reg.inner.slots.lock().expect("slots lock");
                if let Some(Slot::Loading(rx)) = slots.get(line_id)
                    && rx.same_channel(&waiter)
                {
                    slots.remove(line_id);
                }
            }
        }
    }
}

/// The handle of a line that is open now, without loading it.
pub(crate) fn get_open(reg: &Registry, line_id: &str) -> Option<LineHandle> {
    match reg.inner.slots.lock().expect("slots lock").get(line_id) {
        Some(Slot::Open(h)) => Some(h.clone()),
        _ => None,
    }
}

fn open_senders(reg: &Registry) -> Vec<(String, mpsc::UnboundedSender<task::Cmd>)> {
    let slots = reg.inner.slots.lock().expect("slots lock");
    slots
        .iter()
        .filter_map(|(id, s)| match s {
            Slot::Open(h) => Some((id.clone(), h.shared.cmds.clone())),
            Slot::Loading(_) => None,
        })
        .collect()
}

pub(crate) fn open_lines(reg: &Registry) -> Vec<String> {
    let mut ids: Vec<String> = open_senders(reg).into_iter().map(|(id, _)| id).collect();
    ids.sort();
    ids
}

pub(crate) async fn shutdown(reg: &Registry, _state: &AppState) {
    let mut waits = Vec::new();
    for (id, tx) in open_senders(reg) {
        let (rtx, rrx) = oneshot::channel();
        if tx.send(task::Cmd::Stop { save: true, reply: Some(rtx) }).is_ok() {
            waits.push((id, rrx));
        }
    }
    for (id, rx) in waits {
        if tokio::time::timeout(Duration::from_secs(120), rx).await.is_err() {
            tracing::warn!(line = %id, "saving a line on shutdown took too long");
        }
    }
    reg.inner.slots.lock().expect("slots lock").clear();
}

pub(crate) async fn unload(reg: &Registry, line_id: &str) -> bool {
    let tx = {
        let slots = reg.inner.slots.lock().expect("slots lock");
        match slots.get(line_id) {
            Some(Slot::Open(h)) => h.shared.cmds.clone(),
            _ => return false,
        }
    };
    let (rtx, rrx) = oneshot::channel();
    if tx.send(task::Cmd::Unload(rtx)).is_err() {
        return false;
    }
    rrx.await.unwrap_or(false)
}

pub(crate) async fn discard(reg: &Registry, line_id: &str) {
    let tx = {
        let mut slots = reg.inner.slots.lock().expect("slots lock");
        match slots.remove(line_id) {
            Some(Slot::Open(h)) => h.shared.cmds.clone(),
            _ => return,
        }
    };
    reg.inner.states.lock().expect("states lock").forget(line_id);
    let (rtx, rrx) = oneshot::channel();
    if tx.send(task::Cmd::Stop { save: false, reply: Some(rtx) }).is_ok() {
        let _ = rrx.await;
    }
}

/// Every 15 seconds, each open line checks whether it is idle (and unloads), whether a snapshot is due, and drops
/// expired presence.
pub(crate) fn spawn_maintenance(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(15));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            for (_, tx) in open_senders(&state.lines) {
                let _ = tx.send(task::Cmd::Tick);
            }
        }
    });
}
