//! Sends Silicon Calc's notifications to Silicon Ting.
//!
//! Contract (the service's notification worker calls exactly this):
//! `Publisher::new(Config)?.send(&body).await -> Result<Accepted, Error>`, where `body` is one ting
//! (`{"type", "for", "key", "data", "metadata"}`, docs/API.md). Each send carries Calc's App verification proof for
//! Ting (receiving app `ting`, scope `tings.send`), issued by Silicon Accounts with Calc's app credentials and
//! cached until shortly before it expires. Built by the e2e/Ting work item (docs/BUILDING.md).
//!
//! How it talks to Ting (its v1 WebSocket, as every publisher uses it):
//!
//! - One WebSocket (`{ting_url}/v1/ws?protocol=v1`), opened on the first send, reused by every later one and closed
//!   after a few idle minutes. Ting greets with `{"op":"ready","protocol":"v1","receiver_id":…}`.
//! - A send is `{"op":"send","request_id","proof_token","body"}` with the ting as its exact JSON text; Ting answers
//!   `{"op":"accepted","request_id","id","created_at","status":"accepted","key","silent"}` or
//!   `{"op":"error","request_id","error":{"code","message"}}`. Answers are matched to sends by `request_id`, so
//!   several sends may wait on the socket at once.
//! - A connection that breaks fails the sends waiting on it with [`Error::Transport`] (Ting may or may not have
//!   stored them: sending the same ting again is safe, Ting keeps one ting per key) and is opened again for the next
//!   send, after a backoff that grows with each failure in a row (up to 30 s).
//! - The proof is issued once and reused until shortly before it expires. When Ting refuses it (`invalid_proof`
//!   and the like), it is dropped, a new one is issued and the ting is sent once more.
//!
//! Nothing here records telemetry; proofs and the app secret are never logged.

use std::{
    collections::{HashMap, VecDeque},
    fmt,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use futures::{SinkExt as _, StreamExt as _};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use silicon_accounts_client::{AccountsClient, IssueAppVerification};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{
    net::TcpStream,
    sync::{Mutex, mpsc, oneshot},
    time::{Instant, sleep_until, timeout, timeout_at},
};
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, connect_async_tls_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};
use url::Url;

/// The scope Calc's proof for Ting carries.
pub const SEND_SCOPE: &str = "tings.send";
/// The largest ting Ting takes (the body's JSON text).
pub const MAX_BODY_BYTES: usize = 256 * 1024;
const MAX_FRAME_BYTES: usize = 1024 * 1024;
const MAX_KEY_BYTES: usize = 200;
const QUEUE: usize = 256;

#[derive(Clone, Debug)]
pub struct Config {
    /// Silicon Ting's origin (`https://ting.teamofsilicons.com`); the WebSocket is `{ting_url}/v1/ws?protocol=v1`.
    pub ting_url: String,
    /// Where Calc reaches the Silicon Accounts API, to issue its proof.
    pub accounts_api_url: String,
    pub app_id: String,
    pub app_secret: String,
    /// The receiving app of the proof (`ting`).
    pub ting_app_id: String,
    pub allow_http: bool,
}

/// Ting stored the ting.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Accepted {
    pub id: String,
    pub created_at: String,
    pub silent: bool,
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum Error {
    #[error("Silicon Ting could not be reached: {0}")]
    Transport(String),
    #[error("Silicon Ting refused the ting: {code}")]
    Rejected { code: String },
    #[error("Silicon Accounts could not issue Calc's proof for Ting: {0}")]
    Authority(String),
    #[error("Silicon Ting answered something unexpected: {0}")]
    Protocol(String),
    #[error("{0}")]
    Invalid(String),
}

/// Refusals that sending the same ting again can never turn into an acceptance.
const FINAL_REFUSALS: [&str; 5] = ["invalid_ting", "unknown_recipient", "invalid_input", "idempotency_conflict", "payload_too_large"];

/// Refusals of the proof itself: a new proof may be accepted.
const PROOF_REFUSALS: [&str; 6] = ["invalid_proof", "proof_expired", "expired_proof", "unauthorized", "invalid_token", "missing_scope"];

impl Error {
    /// Whether sending the same ting again later may succeed.
    pub fn retryable(&self) -> bool {
        match self {
            Self::Invalid(_) => false,
            Self::Rejected { code } => !FINAL_REFUSALS.contains(&code.as_str()),
            Self::Transport(_) | Self::Authority(_) | Self::Protocol(_) => true,
        }
    }

    /// A short stable code for the outbox (`last_error`): Ting's own code for a refusal.
    pub fn code(&self) -> &str {
        match self {
            Self::Transport(_) => "ting_unreachable",
            Self::Rejected { code } => code,
            Self::Authority(_) => "ting_proof_unavailable",
            Self::Protocol(_) => "ting_protocol_error",
            Self::Invalid(_) => "invalid_ting",
        }
    }
}

/// How the publisher waits and retries. `Options::default()` is what [`Publisher::new`] uses.
#[derive(Clone, Debug)]
pub struct Options {
    /// The longest one send takes, from [`Publisher::send`] to Ting's answer (default 15 s).
    pub request_timeout: Duration,
    /// Opening the WebSocket and receiving Ting's ready frame (default 10 s).
    pub connect_timeout: Duration,
    /// The cached proof is replaced when it has less than this left (default 2 minutes).
    pub proof_refresh_margin: Duration,
    /// The lifetime asked for each proof, 60 to 1800 seconds (default: Silicon Accounts' own, 30 minutes).
    pub proof_ttl_seconds: Option<u32>,
    /// The longest wait between attempts to reconnect (default 30 s).
    pub max_backoff: Duration,
    /// The WebSocket is closed after this long without sends; the next send opens it again (default 5 minutes).
    pub idle_timeout: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(15),
            connect_timeout: Duration::from_secs(10),
            proof_refresh_margin: Duration::from_secs(120),
            proof_ttl_seconds: None,
            max_backoff: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(300),
        }
    }
}

pub struct Publisher {
    inner: Arc<Inner>,
}

struct Inner {
    cfg: Config,
    options: Options,
    ws_url: Url,
    tls: Option<Arc<rustls::ClientConfig>>,
    accounts: AccountsClient,
    proof: Mutex<Option<CachedProof>>,
    driver: StdMutex<Option<mpsc::Sender<Job>>>,
}

struct CachedProof {
    token: Arc<str>,
    replace_at: Instant,
}

impl fmt::Debug for Publisher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Publisher")
            .field("ws_url", &self.inner.ws_url.as_str())
            .field("accounts_api_url", &self.inner.cfg.accounts_api_url)
            .field("app_id", &self.inner.cfg.app_id)
            .field("ting_app_id", &self.inner.cfg.ting_app_id)
            .finish_non_exhaustive()
    }
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// `https://ting.example` → `wss://ting.example/v1/ws?protocol=v1` (a path on the origin is kept in front).
fn websocket_url(ting_url: &str, allow_http: bool) -> Result<Url, Error> {
    let mut url = Url::parse(ting_url.trim()).map_err(|e| {
        Error::Invalid(format!("Silicon Ting's URL '{ting_url}' is not a URL ({e}); give its origin, like https://ting.teamofsilicons.com."))
    })?;
    let secure = match url.scheme() {
        "https" | "wss" => true,
        "http" | "ws" => false,
        other => {
            return Err(Error::Invalid(format!(
                "Silicon Ting's URL '{ting_url}' uses '{other}'; give an https (or, locally, http) origin."
            )));
        }
    };
    if !secure && !allow_http && !is_loopback(&url) {
        return Err(Error::Invalid(format!(
            "Silicon Ting's URL '{ting_url}' is plain http outside this machine; use https (http is only for local stacks)."
        )));
    }
    if url.host().is_none() {
        return Err(Error::Invalid(format!("Silicon Ting's URL '{ting_url}' has no host.")));
    }
    url.set_scheme(if secure { "wss" } else { "ws" })
        .map_err(|()| Error::Invalid(format!("Silicon Ting's URL '{ting_url}' can't be turned into a WebSocket URL.")))?;
    let base = url.path().trim_end_matches('/').to_owned();
    url.set_path(&format!("{base}/v1/ws"));
    url.set_query(Some("protocol=v1"));
    url.set_fragment(None);
    Ok(url)
}

fn tls_config() -> Result<Arc<rustls::ClientConfig>, Error> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::Invalid(format!("TLS for Silicon Ting can't be set up: {e}")))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

impl Publisher {
    pub fn new(cfg: Config) -> Result<Self, Error> {
        Self::with_options(cfg, Options::default())
    }

    /// Like [`Publisher::new`], with other timeouts.
    pub fn with_options(cfg: Config, options: Options) -> Result<Self, Error> {
        if cfg.app_id.trim().is_empty() {
            return Err(Error::Invalid("Calc's app id is empty; it is the app id at Silicon Accounts (calc).".into()));
        }
        if cfg.app_secret.trim().is_empty() {
            return Err(Error::Invalid("Calc's app secret is empty; Calc's proof for Ting is issued with it (sa_app_…).".into()));
        }
        let ting_app = cfg.ting_app_id.trim();
        if ting_app.is_empty() || ting_app.contains(',') || ting_app.chars().any(char::is_whitespace) {
            return Err(Error::Invalid(format!(
                "Ting's app id '{}' must name exactly one app (ting): the receiving app of Calc's proof.",
                cfg.ting_app_id
            )));
        }
        let ws_url = websocket_url(&cfg.ting_url, cfg.allow_http)?;
        let tls = if ws_url.scheme() == "wss" { Some(tls_config()?) } else { None };
        let accounts = AccountsClient::builder()
            .base_url(cfg.accounts_api_url.trim())
            .allow_insecure_http(cfg.allow_http)
            .user_agent("silicon-calc-ting")
            .timeout(options.request_timeout)
            .build()
            .map_err(|e| Error::Invalid(format!("Silicon Accounts' API URL '{}' can't be used: {e}", cfg.accounts_api_url)))?;
        Ok(Self {
            inner: Arc::new(Inner {
                cfg,
                options,
                ws_url,
                tls,
                accounts,
                proof: Mutex::new(None),
                driver: StdMutex::new(None),
            }),
        })
    }

    /// Where the publisher connects (`wss://…/v1/ws?protocol=v1`).
    pub fn websocket_url(&self) -> &str {
        self.inner.ws_url.as_str()
    }

    /// Sends one ting and waits for Ting to accept it.
    pub async fn send(&self, body: &Value) -> Result<Accepted, Error> {
        let ting = prepare(body)?;
        let mut replaced = false;
        loop {
            let proof = self.inner.proof().await?;
            let result = self.inner.send_with(&ting, &proof).await;
            if let Err(Error::Rejected { code }) = &result
                && PROOF_REFUSALS.contains(&code.as_str())
            {
                self.inner.forget(&proof).await;
                if !replaced {
                    tracing::info!(code = %code, "Silicon Ting refused Calc's proof; sending again with a new one");
                    replaced = true;
                    continue;
                }
            }
            return result;
        }
    }
}

/// A ting checked and serialized, ready to go.
struct Prepared {
    text: String,
    key: String,
}

fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

fn is_type_part(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Checks a ting the way Ting will, so a ting that can never be accepted fails here ([`Error::Invalid`]).
fn prepare(body: &Value) -> Result<Prepared, Error> {
    let Some(obj) = body.as_object() else {
        return Err(Error::Invalid("A ting is a JSON object: {\"type\", \"for\", \"key\", \"data\", \"metadata\"}.".into()));
    };
    for k in obj.keys() {
        if !matches!(k.as_str(), "type" | "for" | "key" | "data" | "metadata" | "delivery") {
            return Err(Error::Invalid(format!(
                "A ting has no field \"{k}\"; its fields are type, for, key, data and metadata."
            )));
        }
    }
    let kind = obj.get("type").and_then(Value::as_str).unwrap_or_default();
    let parts: Vec<&str> = kind.split('.').collect();
    if kind.len() > 255 || parts.len() != 3 || !parts.iter().all(|p| is_type_part(p)) {
        return Err(Error::Invalid(format!(
            "\"{kind}\" is not a ting type: a type is {{app}}.{{service}}.{{event}} in lowercase, like calc.mention.received."
        )));
    }
    let recipient = obj.get("for").and_then(Value::as_str).unwrap_or_default();
    if !is_uuid(recipient) {
        return Err(Error::Invalid(format!(
            "The ting's \"for\" is '{recipient}', not an account uuid; a ting names its recipient by Silicon Accounts uuid."
        )));
    }
    let key = obj.get("key").and_then(Value::as_str).unwrap_or_default();
    if key.is_empty() || key.len() > MAX_KEY_BYTES || key.chars().any(char::is_control) {
        return Err(Error::Invalid(format!(
            "The ting's key must be 1 to {MAX_KEY_BYTES} bytes without control characters (it is {} bytes).",
            key.len()
        )));
    }
    if !obj.get("data").is_some_and(Value::is_object) {
        return Err(Error::Invalid("The ting's data must be a JSON object.".into()));
    }
    if obj.get("metadata").is_some_and(|m| !m.is_object()) {
        return Err(Error::Invalid("The ting's metadata must be a JSON object.".into()));
    }
    if obj.get("delivery").is_some_and(|d| d.as_str() != Some("required")) {
        return Err(Error::Invalid("The ting's delivery may only be \"required\".".into()));
    }
    let text = serde_json::to_string(body).map_err(|e| Error::Invalid(format!("The ting can't be written as JSON: {e}")))?;
    if text.len() > MAX_BODY_BYTES {
        return Err(Error::Invalid(format!(
            "The ting is {} bytes; Ting takes at most {MAX_BODY_BYTES}. Send less in its data.",
            text.len()
        )));
    }
    Ok(Prepared { text, key: key.to_owned() })
}

impl Inner {
    /// Calc's proof for Ting: the cached one, or a new one when it is about to expire (one issuance at a time).
    async fn proof(&self) -> Result<Arc<str>, Error> {
        let mut cached = self.proof.lock().await;
        if let Some(p) = cached.as_ref()
            && Instant::now() < p.replace_at
        {
            return Ok(p.token.clone());
        }
        let app = self.accounts.as_app(self.cfg.app_id.trim(), self.cfg.app_secret.trim());
        let request = IssueAppVerification {
            receiving_app: self.cfg.ting_app_id.trim().to_owned(),
            scopes: vec![SEND_SCOPE.to_owned()],
            access_ttl_seconds: self.options.proof_ttl_seconds,
        };
        // An idempotency key, so a retried request after a lost answer gets the same proof back.
        let key = format!("calc-ting-proof-{}", uuid::Uuid::now_v7());
        let issued = timeout(self.options.request_timeout, app.issue_app_verification(&request, Some(&key)))
            .await
            .map_err(|_| {
                Error::Authority(format!("Silicon Accounts did not answer within {} s.", self.options.request_timeout.as_secs()))
            })?
            .map_err(|e| Error::Authority(e.to_string()))?;
        if let Some(receiving) = issued.receiving_app.as_deref()
            && receiving != request.receiving_app
        {
            return Err(Error::Authority(format!(
                "Silicon Accounts issued a proof for {receiving}, not for {}.",
                request.receiving_app
            )));
        }
        if !issued.scopes.is_empty() && !issued.scopes.iter().any(|s| s == SEND_SCOPE) {
            return Err(Error::Authority(format!("Silicon Accounts issued the proof without the {SEND_SCOPE} scope.")));
        }
        let token = issued.proof_token.expose().trim();
        if token.is_empty() {
            return Err(Error::Authority("Silicon Accounts answered without a proof token.".into()));
        }
        let lifetime = issued
            .expires_at
            .map(|at| at - OffsetDateTime::now_utc())
            .and_then(|d| Duration::try_from(d).ok())
            .unwrap_or(Duration::from_secs(u64::from(self.options.proof_ttl_seconds.unwrap_or(1800))));
        // Replace it shortly before it expires; a short-lived proof is used for half its life.
        let keep = lifetime.saturating_sub(self.options.proof_refresh_margin).max(lifetime / 2);
        let token: Arc<str> = Arc::from(token);
        *cached = Some(CachedProof { token: token.clone(), replace_at: Instant::now() + keep });
        tracing::debug!(lifetime_s = lifetime.as_secs(), "issued Calc's proof for Ting");
        Ok(token)
    }

    /// Drops the cached proof (after Ting refused it), unless another send already replaced it.
    async fn forget(&self, token: &Arc<str>) {
        let mut cached = self.proof.lock().await;
        if cached.as_ref().is_some_and(|p| p.token == *token) {
            *cached = None;
        }
    }

    /// The connection task's queue, starting the task when it isn't running.
    fn driver(&self) -> mpsc::Sender<Job> {
        let mut guard = self.driver.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(tx) = guard.as_ref()
            && !tx.is_closed()
        {
            return tx.clone();
        }
        let (tx, rx) = mpsc::channel(QUEUE);
        tokio::spawn(drive(rx, self.ws_url.clone(), self.tls.clone(), self.options.clone()));
        *guard = Some(tx.clone());
        tx
    }

    async fn send_with(&self, ting: &Prepared, proof: &Arc<str>) -> Result<Accepted, Error> {
        let request_id = format!("calc-{}", uuid::Uuid::new_v4().simple());
        let frame = json!({"op": "send", "request_id": request_id, "proof_token": &**proof, "body": ting.text}).to_string();
        if frame.len() > MAX_FRAME_BYTES {
            return Err(Error::Invalid(format!("The ting is too big to send ({} bytes with its proof).", frame.len())));
        }
        let deadline = Instant::now() + self.options.request_timeout;
        let (reply, answer) = oneshot::channel();
        let job = Job { request_id, key: ting.key.clone(), frame, deadline, reply };
        let mut job = Some(job);
        for _ in 0..2 {
            let tx = self.driver();
            match timeout_at(deadline, tx.send(job.take().expect("job"))).await {
                Ok(Ok(())) => break,
                // The task stopped (its runtime ended): start a new one and hand the job over again.
                Ok(Err(mpsc::error::SendError(back))) => job = Some(back),
                Err(_) => return Err(Error::Transport(too_slow(self.options.request_timeout))),
            }
        }
        if job.is_some() {
            return Err(Error::Transport("the publisher's connection task could not be started.".into()));
        }
        // The task answers by the deadline; the extra second covers a task that died with the job.
        match timeout_at(deadline + Duration::from_secs(1), answer).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(Error::Transport("the connection to Silicon Ting ended before it answered; it may or may not have the ting.".into())),
            Err(_) => Err(Error::Transport(too_slow(self.options.request_timeout))),
        }
    }
}

fn too_slow(limit: Duration) -> String {
    format!("Silicon Ting did not answer within {} s; it may or may not have the ting.", limit.as_secs_f32())
}

// ------------------------------------------------------------------------------------------------- connection task

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Job {
    request_id: String,
    key: String,
    frame: String,
    deadline: Instant,
    reply: oneshot::Sender<Result<Accepted, Error>>,
}

impl Job {
    fn finish(self, result: Result<Accepted, Error>) {
        let _ = self.reply.send(result);
    }

    fn abandoned(&self) -> bool {
        self.reply.is_closed()
    }
}

/// Why a connection ended.
enum End {
    /// The publisher was dropped.
    Stopped,
    /// Closed after the idle timeout; nothing was waiting.
    Idle,
    /// It broke (or was reset after a send went unanswered). `worked`: Ting answered at least once on it.
    Broken { worked: bool },
}

fn fail_all(inflight: &mut HashMap<String, Job>, error: &Error) {
    for (_, job) in inflight.drain() {
        job.finish(Err(error.clone()));
    }
}

/// The delay before connecting again after `failures` failures in a row: 250 ms after a connection that worked,
/// then 1 s, 2 s, 4 s … up to `max`, each ±20% so publishers don't come back all at once.
fn backoff(failures: u32, max: Duration) -> Duration {
    let base = if failures == 0 { Duration::from_millis(250) } else { Duration::from_secs(1u64 << (failures - 1).min(6)) };
    let base = base.min(max);
    let jitter = f64::from(uuid::Uuid::new_v4().as_bytes()[0]) / 255.0; // 0..=1
    base.mul_f64(0.8 + 0.4 * jitter).min(max)
}

/// Owns the WebSocket: connects when there is something to send, writes sends, matches answers.
async fn drive(mut jobs: mpsc::Receiver<Job>, ws_url: Url, tls: Option<Arc<rustls::ClientConfig>>, options: Options) {
    let mut queue: VecDeque<Job> = VecDeque::new();
    let mut failures: u32 = 0;
    let mut retry_at: Option<Instant> = None;
    loop {
        if queue.is_empty() {
            match jobs.recv().await {
                Some(job) => queue.push_back(job),
                None => return,
            }
        }
        if let Some(at) = retry_at {
            if !wait_until(at, &mut queue, &mut jobs).await {
                return;
            }
            if queue.is_empty() {
                continue;
            }
        }
        match connect(&ws_url, tls.as_ref(), &options).await {
            Err(error) => {
                failures = failures.saturating_add(1);
                let delay = backoff(failures, options.max_backoff);
                retry_at = Some(Instant::now() + delay);
                tracing::warn!(error = %error, retry_in_ms = delay.as_millis() as u64, "connecting to Silicon Ting failed");
                for job in queue.drain(..) {
                    job.finish(Err(error.clone()));
                }
            }
            Ok(socket) => {
                tracing::debug!(url = %ws_url, "connected to Silicon Ting");
                match connected(socket, &mut queue, &mut jobs, &options).await {
                    End::Stopped => return,
                    End::Idle => {
                        failures = 0;
                        retry_at = None;
                    }
                    End::Broken { worked } => {
                        failures = if worked { 0 } else { failures.saturating_add(1) };
                        retry_at = Some(Instant::now() + backoff(failures, options.max_backoff));
                    }
                }
            }
        }
    }
}

/// Waits for the next connection attempt at `at`, taking new sends meanwhile. A send that can't wait that long fails
/// now. False when the publisher is gone.
async fn wait_until(at: Instant, queue: &mut VecDeque<Job>, jobs: &mut mpsc::Receiver<Job>) -> bool {
    loop {
        let now = Instant::now();
        if now >= at {
            return true;
        }
        let mut kept = VecDeque::with_capacity(queue.len());
        for job in queue.drain(..) {
            if job.abandoned() {
                continue;
            }
            if job.deadline <= at {
                let wait = at.saturating_duration_since(now);
                job.finish(Err(Error::Transport(format!(
                    "the last connection to Silicon Ting failed; the next attempt to connect is in {:.1} s.",
                    wait.as_secs_f32()
                ))));
            } else {
                kept.push_back(job);
            }
        }
        *queue = kept;
        if queue.is_empty() {
            return true;
        }
        tokio::select! {
            () = sleep_until(at) => return true,
            job = jobs.recv() => match job {
                Some(job) => queue.push_back(job),
                None => {
                    for job in queue.drain(..) {
                        job.finish(Err(Error::Transport("the publisher stopped.".into())));
                    }
                    return false;
                }
            },
        }
    }
}

async fn connect(url: &Url, tls: Option<&Arc<rustls::ClientConfig>>, options: &Options) -> Result<Socket, Error> {
    let config = WebSocketConfig::default().max_message_size(Some(MAX_FRAME_BYTES)).max_frame_size(Some(MAX_FRAME_BYTES));
    let connector = Some(match tls {
        Some(cfg) => Connector::Rustls(cfg.clone()),
        None => Connector::Plain,
    });
    let attempt = async {
        let (mut socket, _) = connect_async_tls_with_config(url.as_str(), Some(config), true, connector)
            .await
            .map_err(|e| Error::Transport(format!("opening {url} failed: {e}")))?;
        let ready = next_json(&mut socket).await?;
        if ready.get("op").and_then(Value::as_str) != Some("ready")
            || ready.get("protocol").and_then(Value::as_str) != Some("v1")
            || ready.get("receiver_id").and_then(Value::as_str).is_none_or(str::is_empty)
        {
            let _ = socket.close(None).await;
            return Err(Error::Protocol(format!("Ting greeted with {} instead of its v1 ready frame.", clip(&ready.to_string()))));
        }
        Ok(socket)
    };
    timeout(options.connect_timeout, attempt).await.map_err(|_| {
        Error::Transport(format!("opening {url} took longer than {} s.", options.connect_timeout.as_secs_f32()))
    })?
}

/// The next JSON text frame (answering pings on the way).
async fn next_json(socket: &mut Socket) -> Result<Value, Error> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str(&text).map_err(|_| Error::Protocol(format!("Ting sent a frame that is not JSON: {}", clip(&text))));
            }
            Some(Ok(Message::Ping(payload))) => {
                socket.send(Message::Pong(payload)).await.map_err(|e| Error::Transport(format!("answering Ting's ping failed: {e}")))?;
            }
            Some(Ok(Message::Pong(_) | Message::Frame(_))) => {}
            Some(Ok(Message::Binary(_))) => return Err(Error::Protocol("Ting sent a binary frame; its protocol is JSON text.".into())),
            Some(Ok(Message::Close(frame))) => return Err(Error::Transport(closed_by_ting(frame.as_ref()))),
            Some(Err(e)) => return Err(Error::Transport(format!("the WebSocket to Silicon Ting failed: {e}"))),
            None => return Err(Error::Transport("Silicon Ting closed the connection.".into())),
        }
    }
}

fn closed_by_ting(frame: Option<&tokio_tungstenite::tungstenite::protocol::CloseFrame>) -> String {
    match frame {
        Some(f) if !f.reason.is_empty() => format!("Silicon Ting closed the connection ({}: {}).", u16::from(f.code), f.reason),
        Some(f) => format!("Silicon Ting closed the connection ({}).", u16::from(f.code)),
        None => "Silicon Ting closed the connection.".into(),
    }
}

fn clip(s: &str) -> String {
    if s.len() <= 300 {
        return s.to_owned();
    }
    let mut end = 300;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

async fn sleep_until_some(at: Option<Instant>) {
    match at {
        Some(at) => sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Runs one connection until it breaks, idles out or the publisher is dropped.
async fn connected(mut socket: Socket, queue: &mut VecDeque<Job>, jobs: &mut mpsc::Receiver<Job>, options: &Options) -> End {
    let mut inflight: HashMap<String, Job> = HashMap::new();
    let mut worked = false;
    let mut idle_since = Instant::now();
    loop {
        while let Some(job) = queue.pop_front() {
            if job.abandoned() {
                continue;
            }
            if job.deadline <= Instant::now() {
                job.finish(Err(Error::Transport(too_slow(options.request_timeout))));
                continue;
            }
            let deadline = job.deadline;
            let frame = Message::Text(job.frame.clone().into());
            match timeout_at(deadline, socket.send(frame)).await {
                Ok(Ok(())) => {
                    inflight.insert(job.request_id.clone(), job);
                }
                Ok(Err(e)) => {
                    let error = Error::Transport(format!(
                        "the WebSocket to Silicon Ting broke while sending ({e}); it may or may not have the ting."
                    ));
                    job.finish(Err(error.clone()));
                    fail_all(&mut inflight, &error);
                    return End::Broken { worked };
                }
                Err(_) => {
                    let error = Error::Transport(too_slow(options.request_timeout));
                    job.finish(Err(error.clone()));
                    fail_all(&mut inflight, &error);
                    return End::Broken { worked };
                }
            }
        }
        let next_deadline = inflight.values().map(|j| j.deadline).min();
        let idle_at = inflight.is_empty().then(|| idle_since + options.idle_timeout);
        tokio::select! {
            job = jobs.recv() => match job {
                Some(job) => queue.push_back(job),
                None => {
                    fail_all(&mut inflight, &Error::Transport("the publisher stopped before Ting answered; it may or may not have the ting.".into()));
                    let _ = timeout(Duration::from_secs(2), socket.close(None)).await;
                    return End::Stopped;
                }
            },
            frame = socket.next() => match frame {
                Some(Ok(Message::Text(text))) => match answer(&text, &mut inflight) {
                    Ok(answered) => {
                        worked |= answered;
                        if inflight.is_empty() {
                            idle_since = Instant::now();
                        }
                    }
                    Err(error) => {
                        tracing::warn!(error = %error, "Silicon Ting sent a frame that can't be matched to a send; reconnecting");
                        fail_all(&mut inflight, &error);
                        let _ = timeout(Duration::from_secs(2), socket.close(None)).await;
                        return End::Broken { worked };
                    }
                },
                Some(Ok(Message::Ping(payload))) => {
                    if timeout(options.connect_timeout, socket.send(Message::Pong(payload))).await.map_or(true, |r| r.is_err()) {
                        fail_all(&mut inflight, &Error::Transport("the WebSocket to Silicon Ting broke; it may or may not have the ting.".into()));
                        return End::Broken { worked };
                    }
                }
                Some(Ok(Message::Pong(_) | Message::Frame(_))) => {}
                Some(Ok(Message::Binary(_))) => {
                    fail_all(&mut inflight, &Error::Protocol("Ting sent a binary frame; its protocol is JSON text.".into()));
                    let _ = timeout(Duration::from_secs(2), socket.close(None)).await;
                    return End::Broken { worked };
                }
                Some(Ok(Message::Close(frame))) => {
                    let reason = closed_by_ting(frame.as_ref());
                    tracing::debug!(%reason, "Silicon Ting closed the connection");
                    fail_all(&mut inflight, &Error::Transport(format!("{reason} It may or may not have the ting.")));
                    return End::Broken { worked };
                }
                Some(Err(e)) => {
                    fail_all(&mut inflight, &Error::Transport(format!("the WebSocket to Silicon Ting failed ({e}); it may or may not have the ting.")));
                    return End::Broken { worked };
                }
                None => {
                    fail_all(&mut inflight, &Error::Transport("Silicon Ting closed the connection; it may or may not have the ting.".into()));
                    return End::Broken { worked };
                }
            },
            () = sleep_until_some(next_deadline) => {
                // A send went unanswered: answer it, and don't trust this connection with the others.
                let now = Instant::now();
                let late: Vec<String> = inflight.iter().filter(|(_, j)| j.deadline <= now).map(|(k, _)| k.clone()).collect();
                for id in late {
                    if let Some(job) = inflight.remove(&id) {
                        job.finish(Err(Error::Transport(too_slow(options.request_timeout))));
                    }
                }
                fail_all(&mut inflight, &Error::Transport("the connection to Silicon Ting was reset after a send went unanswered; it may or may not have the ting.".into()));
                let _ = timeout(Duration::from_secs(2), socket.close(None)).await;
                return End::Broken { worked };
            },
            () = sleep_until_some(idle_at) => {
                let _ = timeout(Duration::from_secs(2), socket.close(None)).await;
                return End::Idle;
            },
        }
    }
}

/// Hands Ting's answer to the send it belongs to. Ok(true): a send was answered; Ok(false): an answer to a send
/// that already gave up (ignored). Err: a frame that belongs to no send, so the connection can't be trusted.
fn answer(text: &str, inflight: &mut HashMap<String, Job>) -> Result<bool, Error> {
    let value: Value = serde_json::from_str(text).map_err(|_| Error::Protocol(format!("Ting sent a frame that is not JSON: {}", clip(text))))?;
    let Some(request_id) = value.get("request_id").and_then(Value::as_str) else {
        return Err(Error::Protocol(format!("Ting sent a frame without a request_id: {}", clip(text))));
    };
    let Some(job) = inflight.remove(request_id) else {
        tracing::debug!(request_id, "an answer from Silicon Ting came after its send gave up");
        return Ok(false);
    };
    let result = accepted(&value, &job.key);
    job.finish(result);
    Ok(true)
}

/// Reads one answer to a send.
fn accepted(value: &Value, key: &str) -> Result<Accepted, Error> {
    match value.get("op").and_then(Value::as_str) {
        Some("accepted") => {
            if let Some(status) = value.get("status")
                && status.as_str() != Some("accepted")
            {
                return Err(Error::Protocol(format!("Ting answered accepted with status {status}.")));
            }
            let got = value.get("key").and_then(Value::as_str).unwrap_or_default();
            if got != key {
                return Err(Error::Protocol(format!("Ting accepted the key '{got}' for a ting whose key is '{key}'.")));
            }
            let id = value
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty() && id.len() <= 255 && !id.chars().any(char::is_control))
                .ok_or_else(|| Error::Protocol(format!("Ting's acceptance has no usable id: {}", clip(&value.to_string()))))?;
            let created_at = value
                .get("created_at")
                .and_then(Value::as_str)
                .filter(|t| OffsetDateTime::parse(t, &Rfc3339).is_ok())
                .ok_or_else(|| Error::Protocol(format!("Ting's acceptance has no RFC 3339 created_at: {}", clip(&value.to_string()))))?;
            let silent = value
                .get("silent")
                .and_then(Value::as_bool)
                .ok_or_else(|| Error::Protocol(format!("Ting's acceptance does not say whether it is silent: {}", clip(&value.to_string()))))?;
            Ok(Accepted { id: id.to_owned(), created_at: created_at.to_owned(), silent })
        }
        Some("error") => {
            let error = value.get("error").cloned().unwrap_or(Value::Null);
            let code = error
                .get("code")
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty() && c.len() <= 100 && c.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
                .unwrap_or("ting_rejected")
                .to_owned();
            let message = error.get("message").and_then(Value::as_str).unwrap_or_default();
            tracing::info!(code = %code, message = %clip(message), "Silicon Ting refused a ting");
            Err(Error::Rejected { code })
        }
        other => Err(Error::Protocol(format!("Ting answered a send with op {other:?}."))),
    }
}

#[cfg(test)]
mod unit {
    use super::*;

    fn ting() -> Value {
        json!({"type": "calc.mention.received", "for": "6667d4b4-1f0b-4c8e-9a52-0d0d6f8c2a11", "key": "k-1",
               "data": {"sheet": {"id": "k3j9xq2mpa7d", "title": "Q3"}}, "metadata": {}})
    }

    #[test]
    fn websocket_urls() {
        let u = |s: &str, http: bool| websocket_url(s, http).map(|u| u.to_string());
        assert_eq!(u("https://ting.teamofsilicons.com", false).unwrap(), "wss://ting.teamofsilicons.com/v1/ws?protocol=v1");
        assert_eq!(u("https://ting.teamofsilicons.com/", false).unwrap(), "wss://ting.teamofsilicons.com/v1/ws?protocol=v1");
        assert_eq!(u("http://127.0.0.1:8735", false).unwrap(), "ws://127.0.0.1:8735/v1/ws?protocol=v1");
        assert_eq!(u("http://localhost:8735/", false).unwrap(), "ws://localhost:8735/v1/ws?protocol=v1");
        assert_eq!(u("https://example.com/ting/", false).unwrap(), "wss://example.com/ting/v1/ws?protocol=v1");
        assert_eq!(u("http://ting.internal:8735", true).unwrap(), "ws://ting.internal:8735/v1/ws?protocol=v1");
        assert!(matches!(u("http://ting.example.com", false), Err(Error::Invalid(_))));
        assert!(matches!(u("ftp://ting.example.com", true), Err(Error::Invalid(_))));
        assert!(matches!(u("not a url", true), Err(Error::Invalid(_))));
    }

    #[test]
    fn prepare_checks_the_ting() {
        let ok = prepare(&ting()).unwrap();
        assert_eq!(ok.key, "k-1");
        assert_eq!(serde_json::from_str::<Value>(&ok.text).unwrap(), ting());
        let without_metadata = json!({"type": "calc.sheet.shared", "for": "6667d4b4-1f0b-4c8e-9a52-0d0d6f8c2a11", "key": "x", "data": {}});
        assert!(prepare(&without_metadata).is_ok());
        let bad = |patch: Value| {
            let mut t = ting();
            for (k, v) in patch.as_object().unwrap() {
                if v.is_null() {
                    t.as_object_mut().unwrap().remove(k);
                } else {
                    t[k] = v.clone();
                }
            }
            prepare(&t).err().unwrap()
        };
        for patch in [
            json!({"type": null}),
            json!({"type": "calc.mention"}),
            json!({"type": "Calc.mention.received"}),
            json!({"type": "calc.mention.received.again"}),
            json!({"for": "c:ada"}),
            json!({"for": null}),
            json!({"key": ""}),
            json!({"key": "a\nb"}),
            json!({"key": "k".repeat(201)}),
            json!({"data": []}),
            json!({"data": null}),
            json!({"metadata": "x"}),
            json!({"org_id": "tos"}),
            json!({"id": "ting_1"}),
            json!({"delivery": "eventually"}),
            json!({"data": {"blob": "x".repeat(MAX_BODY_BYTES)}}),
        ] {
            let e = bad(patch.clone());
            assert!(matches!(e, Error::Invalid(_)), "{patch} → {e:?}");
            assert!(!e.retryable(), "{patch}");
        }
        assert!(prepare(&json!([1])).is_err());
    }

    #[test]
    fn retryable_and_codes() {
        assert!(Error::Transport("x".into()).retryable());
        assert!(Error::Authority("x".into()).retryable());
        assert!(Error::Protocol("x".into()).retryable());
        assert!(!Error::Invalid("x".into()).retryable());
        for code in ["invalid_ting", "unknown_recipient", "invalid_input", "idempotency_conflict", "payload_too_large"] {
            assert!(!Error::Rejected { code: code.into() }.retryable(), "{code}");
        }
        for code in ["invalid_proof", "unavailable", "slow_down", "internal", "permission_denied", "ting_rejected"] {
            assert!(Error::Rejected { code: code.into() }.retryable(), "{code}");
        }
        assert_eq!(Error::Rejected { code: "unknown_recipient".into() }.code(), "unknown_recipient");
        assert_eq!(Error::Transport("x".into()).code(), "ting_unreachable");
    }

    #[test]
    fn answers() {
        let ok = json!({"op": "accepted", "request_id": "r", "id": "ting_1", "created_at": "2026-10-17T03:45:33.114Z",
                        "status": "accepted", "key": "k", "silent": false});
        assert_eq!(accepted(&ok, "k").unwrap(), Accepted { id: "ting_1".into(), created_at: "2026-10-17T03:45:33.114Z".into(), silent: false });
        assert!(matches!(accepted(&ok, "other"), Err(Error::Protocol(_))));
        let mut no_silent = ok.clone();
        no_silent.as_object_mut().unwrap().remove("silent");
        assert!(matches!(accepted(&no_silent, "k"), Err(Error::Protocol(_))));
        let mut bad_time = ok.clone();
        bad_time["created_at"] = json!("yesterday");
        assert!(matches!(accepted(&bad_time, "k"), Err(Error::Protocol(_))));
        let refused = json!({"op": "error", "request_id": "r", "error": {"code": "unknown_recipient", "message": "no"}});
        assert!(matches!(accepted(&refused, "k"), Err(Error::Rejected { code }) if code == "unknown_recipient"));
        let odd = json!({"op": "error", "request_id": "r", "error": {"code": "Not A Code"}});
        assert!(matches!(accepted(&odd, "k"), Err(Error::Rejected { code }) if code == "ting_rejected"));
        assert!(matches!(accepted(&json!({"op": "tings"}), "k"), Err(Error::Protocol(_))));
    }

    #[test]
    fn backoff_grows_and_stays_bounded() {
        let max = Duration::from_secs(30);
        assert!(backoff(0, max) <= Duration::from_millis(300));
        assert!(backoff(1, max) >= Duration::from_millis(800) && backoff(1, max) <= Duration::from_millis(1200));
        assert!(backoff(3, max) >= Duration::from_millis(3200) && backoff(3, max) <= Duration::from_millis(4800));
        for n in 6..40 {
            assert!(backoff(n, max) <= max);
        }
    }

    #[test]
    fn new_checks_the_config() {
        let cfg = Config {
            ting_url: "http://127.0.0.1:8735".into(),
            accounts_api_url: "http://127.0.0.1:9989".into(),
            app_id: "calc".into(),
            app_secret: "sa_app_calc_x".into(),
            ting_app_id: "ting".into(),
            allow_http: true,
        };
        let p = Publisher::new(cfg.clone()).unwrap();
        assert_eq!(p.websocket_url(), "ws://127.0.0.1:8735/v1/ws?protocol=v1");
        assert!(!format!("{p:?}").contains("sa_app_calc_x"));
        assert!(matches!(Publisher::new(Config { app_secret: " ".into(), ..cfg.clone() }), Err(Error::Invalid(_))));
        assert!(matches!(Publisher::new(Config { ting_app_id: "ting, dm".into(), ..cfg.clone() }), Err(Error::Invalid(_))));
        assert!(matches!(Publisher::new(Config { ting_url: "http://ting.example.com".into(), allow_http: false, ..cfg.clone() }), Err(Error::Invalid(_))));
        assert!(Publisher::new(Config { ting_url: "https://ting.teamofsilicons.com".into(), ..cfg }).is_ok());
    }
}
