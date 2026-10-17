//! Test harness: a stand-in Silicon Accounts (JWKS, account lookups, userinfo) that signs real EdDSA tokens,
//! a fresh Postgres database per test, and the service running in-process on a random port.
//!
//! ```ignore
//! mod support;
//! let h = support::Harness::start().await;
//! let ada = h.carbon("c:ada", "Ada");
//! let r = h.post(&ada, "/v1/sheets", json!({"title": "Q3"})).await;
//! ```
#![allow(dead_code)]

use std::{
    collections::HashMap,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
};
use base64::Engine as _;
use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::{Value, json};
use uuid::Uuid;

const KID: &str = "test-key-1";

/// One account the stand-in knows.
#[derive(Clone, Debug)]
pub struct TestAccount {
    pub uuid: Uuid,
    pub id: String,
    pub kind: String,
    pub display_name: String,
    pub email: Option<String>,
    pub token: String,
}

#[derive(Default)]
struct Known {
    by_uuid: HashMap<Uuid, TestAccount>,
}

#[derive(Clone)]
struct MockState {
    known: Arc<Mutex<Known>>,
    key: Arc<SigningKey>,
}

/// The stand-in Silicon Accounts.
pub struct MockAccounts {
    pub url: String,
    state: MockState,
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn summary(a: &TestAccount) -> Value {
    json!({"uuid": a.uuid.to_string(), "kind": a.kind, "id": a.id, "display_name": a.display_name,
           "pfp_url": format!("https://iris.example/pfp/{}", a.uuid), "status": "active"})
}

fn not_found() -> axum::response::Response {
    (StatusCode::NOT_FOUND, Json(json!({"error": {"code": "not_found", "message": "No such account.", "hint": ""}}))).into_response()
}

impl MockAccounts {
    pub async fn start() -> Self {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let state = MockState { known: Arc::default(), key: Arc::new(key) };
        let app = Router::new()
            .route("/.well-known/jwks.json", get(jwks))
            .route("/v1/accounts/by-id/{id}", get(by_id))
            .route("/v1/accounts/{uuid}", get(by_uuid))
            .route("/v1/userinfo", get(userinfo))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr: SocketAddr = listener.local_addr().expect("addr");
        tokio::spawn(async move { axum::serve(listener, app).await.expect("mock accounts") });
        Self { url: format!("http://127.0.0.1:{}", addr.port()), state }
    }

    /// A token for `account` (aud `calc`, 30 minutes).
    pub fn mint(&self, uuid: Uuid, id: &str, kind: &str) -> String {
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let header = json!({"alg": "EdDSA", "typ": "JWT", "kid": KID});
        let claims = json!({"iss": self.url, "sub": uuid.to_string(), "aud": "calc", "exp": now + 1800, "iat": now,
                            "kind": kind, "id": id, "scope": "profile email"});
        let signing_input = format!("{}.{}", b64(header.to_string().as_bytes()), b64(claims.to_string().as_bytes()));
        let sig = self.state.key.sign(signing_input.as_bytes());
        format!("{signing_input}.{}", b64(&sig.to_bytes()))
    }

    /// Adds an account and returns it with a token.
    pub fn add(&self, id: &str, display_name: &str, email: Option<&str>) -> TestAccount {
        let kind = if id.starts_with("si:") { "silicon" } else { "carbon" };
        let uuid = Uuid::new_v4();
        let token = self.mint(uuid, id, kind);
        let a = TestAccount {
            uuid,
            id: id.to_owned(),
            kind: kind.to_owned(),
            display_name: display_name.to_owned(),
            email: email.map(str::to_owned),
            token,
        };
        self.state.known.lock().expect("lock").by_uuid.insert(uuid, a.clone());
        a
    }
}

async fn jwks(State(s): State<MockState>) -> impl IntoResponse {
    let x = b64(s.key.verifying_key().as_bytes());
    Json(json!({"keys": [{"kty": "OKP", "crv": "Ed25519", "x": x, "kid": KID, "use": "sig", "alg": "EdDSA"}]}))
}

async fn by_uuid(State(s): State<MockState>, Path(uuid): Path<String>) -> axum::response::Response {
    let Ok(u) = Uuid::parse_str(&uuid) else { return not_found() };
    match s.known.lock().expect("lock").by_uuid.get(&u) {
        Some(a) => Json(summary(a)).into_response(),
        None => not_found(),
    }
}

async fn by_id(State(s): State<MockState>, Path(id): Path<String>) -> axum::response::Response {
    let known = s.known.lock().expect("lock");
    match known.by_uuid.values().find(|a| a.id.eq_ignore_ascii_case(&id)) {
        Some(a) => Json(summary(a)).into_response(),
        None => not_found(),
    }
}

async fn userinfo(State(s): State<MockState>, headers: HeaderMap) -> axum::response::Response {
    let token = headers.get("authorization").and_then(|h| h.to_str().ok()).and_then(|h| h.strip_prefix("Bearer ")).unwrap_or("");
    let known = s.known.lock().expect("lock");
    match known.by_uuid.values().find(|a| a.token == token) {
        Some(a) => {
            let mut v = summary(a);
            v["membership_id"] = json!(format!("calc:{}", a.uuid));
            v["sub"] = json!(a.uuid.to_string());
            v["email"] = json!(a.email);
            v["email_verified"] = json!(a.email.is_some());
            Json(v).into_response()
        }
        None => (StatusCode::UNAUTHORIZED, Json(json!({"error": {"code": "invalid_token", "message": "bad token"}}))).into_response(),
    }
}

/// A fresh database on the test cluster (`CALC_TEST_DATABASE_URL`, default the local cluster on 5482).
pub async fn fresh_database() -> String {
    let admin = std::env::var("CALC_TEST_DATABASE_URL").unwrap_or_else(|_| "postgres://postgres@127.0.0.1:5482/postgres".into());
    let name = format!("calc_test_{}", Uuid::new_v4().simple());
    let pool = sqlx::PgPool::connect(&admin).await.expect("connect to the test Postgres (is the cluster on 5482 running?)");
    sqlx::query(sqlx::AssertSqlSafe(format!("create database {name}"))).execute(&pool).await.expect("create database");
    pool.close().await;
    match admin.rsplit_once('/') {
        Some((base, _)) => format!("{base}/{name}"),
        None => admin,
    }
}

/// The service, running in-process against the stand-in Accounts and a fresh database.
pub struct Harness {
    pub accounts: MockAccounts,
    pub url: String,
    pub state: calc_api::state::AppState,
    pub http: reqwest::Client,
    pub blob_dir: PathBuf,
}

impl Harness {
    pub async fn start() -> Self {
        let accounts = MockAccounts::start().await;
        let database_url = fresh_database().await;
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
            ting_url: None,
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
        calc_api::start_background(&state);
        let app = calc_api::router(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
        Self { accounts, url: format!("http://127.0.0.1:{port}"), state, http: reqwest::Client::new(), blob_dir }
    }

    pub fn carbon(&self, id: &str, name: &str) -> TestAccount {
        self.accounts.add(id, name, Some(&format!("{}@example.test", id.trim_start_matches("c:"))))
    }

    pub fn silicon(&self, id: &str, name: &str) -> TestAccount {
        self.accounts.add(id, name, None)
    }

    pub async fn get(&self, who: &TestAccount, path: &str) -> (StatusCode, Value) {
        let r = self.http.get(format!("{}{path}", self.url)).bearer_auth(&who.token).send().await.expect("request");
        status_json(r).await
    }

    pub async fn get_anon(&self, path: &str) -> (StatusCode, Value) {
        let r = self.http.get(format!("{}{path}", self.url)).send().await.expect("request");
        status_json(r).await
    }

    pub async fn post(&self, who: &TestAccount, path: &str, body: Value) -> (StatusCode, Value) {
        let r = self.http.post(format!("{}{path}", self.url)).bearer_auth(&who.token).json(&body).send().await.expect("request");
        status_json(r).await
    }

    pub async fn patch(&self, who: &TestAccount, path: &str, body: Value) -> (StatusCode, Value) {
        let r = self.http.patch(format!("{}{path}", self.url)).bearer_auth(&who.token).json(&body).send().await.expect("request");
        status_json(r).await
    }

    pub async fn put(&self, who: &TestAccount, path: &str, body: Value) -> (StatusCode, Value) {
        let r = self.http.put(format!("{}{path}", self.url)).bearer_auth(&who.token).json(&body).send().await.expect("request");
        status_json(r).await
    }

    pub async fn delete(&self, who: &TestAccount, path: &str) -> (StatusCode, Value) {
        let r = self.http.delete(format!("{}{path}", self.url)).bearer_auth(&who.token).send().await.expect("request");
        status_json(r).await
    }

    /// Raw body upload (imports).
    pub async fn upload(&self, who: &TestAccount, path: &str, bytes: Vec<u8>) -> (StatusCode, Value) {
        let r = self.http.post(format!("{}{path}", self.url)).bearer_auth(&who.token).body(bytes).send().await.expect("request");
        status_json(r).await
    }

    pub async fn download(&self, who: &TestAccount, path: &str) -> (StatusCode, Vec<u8>) {
        let r = self.http.get(format!("{}{path}", self.url)).bearer_auth(&who.token).send().await.expect("request");
        let s = StatusCode::from_u16(r.status().as_u16()).expect("status");
        (s, r.bytes().await.expect("body").to_vec())
    }
}

async fn status_json(r: reqwest::Response) -> (StatusCode, Value) {
    let s = StatusCode::from_u16(r.status().as_u16()).expect("status");
    let text = r.text().await.unwrap_or_default();
    let v = if text.is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::String(text)) };
    (s, v)
}
