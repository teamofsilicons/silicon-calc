//! What the platform tests share (sheets, folders, sharing, transfers, search, comments, the Ting sender), on top of
//! `support` (the stand-in Silicon Accounts, a fresh database per test, the service in-process).
//!
//! Every helper goes through the HTTP API or the cross-module contracts (`lines::Registry::open`, `LineHandle`,
//! `notify`), never through a module's private parts, so the same tests hold whatever is behind those contracts.
#![allow(dead_code)]

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::http::StatusCode;
use calc_api::lines::{ApplyRequest, OpKind};
use calc_engine::{Workbook, ops::CellWrite};
use futures::future::BoxFuture;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::support::{self, Harness, MockAccounts, TestAccount};

/// How to start the service for a test.
pub struct Options {
    /// Start the background tasks (the Ting sender, line maintenance), as the service does.
    pub background: bool,
    pub ting_url: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self { background: true, ting_url: None }
    }
}

/// The service with `opts`, against the stand-in Accounts and a fresh database.
pub async fn start_with(opts: Options) -> Harness {
    let accounts = MockAccounts::start().await;
    let database_url = support::fresh_database().await;
    let blob_dir = std::env::temp_dir().join(format!("calc-test-blobs-{}", Uuid::new_v4().simple()));
    let cfg = calc_api::config::Config {
        bind: "127.0.0.1:0".parse().expect("addr"),
        database_url,
        accounts_url: accounts.url.clone(),
        accounts_api_url: accounts.url.clone(),
        app_id: "calc".into(),
        app_secret: "sa_app_calc_test".into(),
        web_url: "http://127.0.0.1:8731".into(),
        blob_dir: blob_dir.clone(),
        ting_url: opts.ting_url,
        ting_app_id: "ting".into(),
        runner_python: std::env::var("CALC_RUNNER_PYTHON").unwrap_or_else(|_| "python3".into()),
        runner_node: std::env::var("CALC_RUNNER_NODE").unwrap_or_else(|_| "node".into()),
        code_sandbox: std::env::var("CALC_CODE_SANDBOX").unwrap_or_else(|_| "auto".into()),
        code_timeout: Duration::from_secs(60),
        max_upload_bytes: 500 << 20,
        snapshot_every: 50,
        line_idle: Duration::from_secs(600),
        allow_http: true,
    };
    let state = calc_api::build_state(cfg).await.expect("state");
    if opts.background {
        calc_api::start_background(&state);
    }
    let app = calc_api::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    Harness { accounts, url: format!("http://127.0.0.1:{port}"), state, http: reqwest::Client::new(), blob_dir }
}

/// Asserts a status, showing the body when it differs.
#[track_caller]
pub fn expect(r: &(StatusCode, Value), status: u16) -> &Value {
    assert_eq!(r.0.as_u16(), status, "expected {status}, got {} with {}", r.0, r.1);
    &r.1
}

/// Asserts an error answer: the status, the code, and that it says what happened and what to do.
#[track_caller]
pub fn expect_error(r: &(StatusCode, Value), status: u16, code: &str) -> Value {
    let body = expect(r, status);
    let e = &body["error"];
    assert_eq!(e["code"], json!(code), "error code in {body}");
    let message = e["message"].as_str().unwrap_or("");
    let hint = e["hint"].as_str().unwrap_or("");
    assert!(!message.is_empty() && !hint.is_empty(), "an error says what happened and what to do: {body}");
    for word in ["user", "human", " AI ", "agent", " org", "team"] {
        let lower = format!(" {} {} ", message.to_lowercase(), hint.to_lowercase());
        assert!(!lower.contains(&word.to_lowercase()), "\"{word}\" isn't one of Calc's words: {body}");
    }
    e.clone()
}

/// Creates a sheet and answers its JSON.
pub async fn new_sheet(h: &Harness, who: &TestAccount, title: &str, folder: Option<&str>) -> Value {
    let mut body = json!({"title": title});
    if let Some(f) = folder {
        body["folder_id"] = json!(f);
    }
    expect(&h.post(who, "/v1/sheets", body).await, 201).clone()
}

pub async fn new_sheet_id(h: &Harness, who: &TestAccount, title: &str, folder: Option<&str>) -> String {
    new_sheet(h, who, title, folder).await["id"].as_str().expect("id").to_owned()
}

/// Creates a folder and answers its id.
pub async fn new_folder(h: &Harness, who: &TestAccount, name: &str, parent: Option<&str>) -> String {
    let mut body = json!({"name": name});
    if let Some(p) = parent {
        body["parent_id"] = json!(p);
    }
    let r = h.post(who, "/v1/folders", body).await;
    expect(&r, 201)["id"].as_str().expect("id").to_owned()
}

/// Shares a sheet (`sheets/{id}`) or folder (`folders/{id}`) with ids or emails.
pub async fn share(h: &Harness, who: &TestAccount, item: &str, invite: &[&str], role: &str) -> Value {
    let r = h.post(who, &format!("/v1/{item}/sharing"), json!({"invite": invite, "role": role})).await;
    expect(&r, 200).clone()
}

/// Signs in to Calc once (the first request records the account and turns its email invites into memberships).
pub async fn sign_in(h: &Harness, who: &TestAccount) {
    expect(&h.get(who, "/v1/me").await, 200);
}

/// The role `who` has on a sheet, or the status they get.
pub async fn role_on(h: &Harness, who: &TestAccount, sheet: &str) -> Result<String, u16> {
    let r = h.get(who, &format!("/v1/sheets/{sheet}")).await;
    if r.0.as_u16() == 200 { Ok(r.1["role"].as_str().unwrap_or("").to_owned()) } else { Err(r.0.as_u16()) }
}

/// The ids of a list of sheets or folders.
pub fn ids(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x["id"].as_str().map(str::to_owned)).collect()).unwrap_or_default()
}

/// One outbox row, as the tests read it.
#[derive(Clone, Debug)]
pub struct Ting {
    pub id: i64,
    pub kind: String,
    pub recipient: Uuid,
    pub key: String,
    pub body: Value,
    pub status: String,
    pub attempts: i32,
    pub last_error: Option<String>,
    pub ting_id: Option<String>,
}

/// Every outbox row, oldest first.
pub async fn outbox(h: &Harness) -> Vec<Ting> {
    let rows: Vec<(i64, String, Uuid, String, Value, String, i32, Option<String>, Option<String>)> = sqlx::query_as(
        "select id, type, recipient, key, body, status, attempts, last_error, ting_id from outbox order by id",
    )
    .fetch_all(&h.state.db)
    .await
    .expect("outbox");
    rows.into_iter()
        .map(|(id, kind, recipient, key, body, status, attempts, last_error, ting_id)| Ting {
            id,
            kind,
            recipient,
            key,
            body,
            status,
            attempts,
            last_error,
            ting_id,
        })
        .collect()
}

/// The outbox rows of one type for one account.
pub async fn tings_for(h: &Harness, kind: &str, to: &TestAccount) -> Vec<Ting> {
    outbox(h).await.into_iter().filter(|t| t.kind == kind && t.recipient == to.uuid).collect()
}

/// `(type, recipient id)` of every outbox row, for comparing what was sent.
pub async fn sent_to(h: &Harness, accounts: &[&TestAccount]) -> Vec<(String, String)> {
    outbox(h)
        .await
        .into_iter()
        .map(|t| {
            let who = accounts.iter().find(|a| a.uuid == t.recipient).map(|a| a.id.clone()).unwrap_or_else(|| t.recipient.to_string());
            (t.kind, who)
        })
        .collect()
}

/// The main line's id.
pub fn main_line(sheet: &str) -> String {
    calc_api::ids::main_line(sheet)
}

/// Writes inputs into a tab of a line by position (`(row, col, input)`, 0-based), through the lines contract.
pub async fn write_cells(h: &Harness, who: &TestAccount, line: &str, tab: u32, cells: &[(u32, u32, &str)]) {
    let writes: Vec<(u32, u32, CellWrite)> =
        cells.iter().map(|&(r, c, input)| (r, c, CellWrite { input: Some(input.to_owned()), ..Default::default() })).collect();
    apply(h, who, line, move |wb: &Workbook| wb.prepare_writes(tab, writes)).await;
}

/// Inserts (`n` > 0) or deletes (`n` < 0) rows or columns at a position, through the lines contract.
pub async fn insert_or_delete(h: &Harness, who: &TestAccount, line: &str, tab: u32, rows: bool, at: u32, n: i32) {
    apply(h, who, line, move |wb: &Workbook| {
        if n >= 0 { wb.prepare_insert(tab, rows, at, n as u32) } else { wb.prepare_delete(tab, rows, at, n.unsigned_abs()) }
    })
    .await;
}

/// Applies one engine operation to a line as `who`, through the lines contract.
pub async fn apply<F>(h: &Harness, who: &TestAccount, line: &str, build: F)
where
    F: FnOnce(&Workbook) -> Result<calc_engine::Op, calc_engine::OpError> + Send + 'static,
{
    let handle = h.state.lines.open(&h.state, line).await.expect("open the line");
    handle
        .apply(ApplyRequest {
            author: who.uuid,
            kind: OpKind::Edit,
            summary: None,
            base_version: None,
            client_id: None,
            build: Box::new(move |wb: &Workbook| build(wb).map(Some).map_err(calc_api::error::ApiError::from)),
        })
        .await
        .expect("apply");
}

/// The first tab's id on a line.
pub async fn first_tab(h: &Harness, line: &str) -> u32 {
    let handle = h.state.lines.open(&h.state, line).await.expect("open the line");
    handle.read(|wb: &Workbook, _| wb.tabs[0].id).await.expect("read")
}

/// Waits until `check` holds (polling), for at most `secs`.
pub async fn eventually<F, Fut>(secs: u64, mut check: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let until = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        if check().await {
            return true;
        }
        if std::time::Instant::now() > until {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A stand-in for Silicon Ting: records every ting it is handed and answers what `answer` says for each.
pub struct FakeTing {
    pub got: Mutex<Vec<Value>>,
    answer: Box<dyn Fn(usize, &Value) -> Result<calc_ting::Accepted, calc_ting::Error> + Send + Sync>,
    pub delay: Duration,
}

impl FakeTing {
    /// Accepts everything.
    pub fn accepting() -> Arc<Self> {
        Self::answering(|n, _| Ok(accepted(n)))
    }

    /// Answers with `answer(n, body)` for the n-th send (0-based).
    pub fn answering(
        answer: impl Fn(usize, &Value) -> Result<calc_ting::Accepted, calc_ting::Error> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self { got: Mutex::new(Vec::new()), answer: Box::new(answer), delay: Duration::ZERO })
    }

    pub fn slow(delay: Duration) -> Arc<Self> {
        Arc::new(Self { got: Mutex::new(Vec::new()), answer: Box::new(|n, _| Ok(accepted(n))), delay })
    }

    pub fn keys(&self) -> Vec<String> {
        self.got.lock().expect("lock").iter().map(|b| b["key"].as_str().unwrap_or("").to_owned()).collect()
    }
}

pub fn accepted(n: usize) -> calc_ting::Accepted {
    calc_ting::Accepted { id: format!("ting-{n}"), created_at: "2026-10-17T00:00:00Z".into(), silent: false }
}

impl calc_api::notify::TingSender for FakeTing {
    fn send<'a>(&'a self, body: &'a Value) -> BoxFuture<'a, Result<calc_ting::Accepted, calc_ting::Error>> {
        Box::pin(async move {
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            let n = {
                let mut got = self.got.lock().expect("lock");
                got.push(body.clone());
                got.len() - 1
            };
            (self.answer)(n, body)
        })
    }
}

/// Queues a ting the way every module does (in a transaction of its own here).
pub async fn enqueue(h: &Harness, kind: &str, to: Uuid, key: &str, data: Value) {
    let mut tx = h.state.db.begin().await.expect("begin");
    calc_api::notify::enqueue(&mut tx, kind, to, key, data).await.expect("enqueue");
    tx.commit().await.expect("commit");
}
