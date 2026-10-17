//! The client against a small stand-in service: refreshing after a 401, waiting out a 429, streaming uploads
//! and downloads, and the live stream reconnecting from the last version it saw.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use futures::StreamExt;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use silicon_calc_client::{
    Client, DownloadQuery, Event, EventOptions, Format, ImportSheet, Line, NewSheet, TokenSource, Upload,
};
use tokio::sync::Mutex;

#[derive(Clone, Default)]
struct Mock {
    creates: Arc<AtomicUsize>,
    connections: Arc<AtomicUsize>,
    sinces: Arc<Mutex<Vec<Option<String>>>>,
}

fn bearer(h: &HeaderMap) -> String {
    h.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").trim_start_matches("Bearer ").to_owned()
}

fn signed_out() -> axum::response::Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": {"code": "signed_out", "message": "This request has no valid sign-in.", "hint": "calc login"}})),
    )
        .into_response()
}

async fn me(headers: HeaderMap) -> axum::response::Response {
    if bearer(&headers) != "fresh" {
        return signed_out();
    }
    Json(json!({"account": {"uuid": "u1", "id": "si:analyst", "kind": "silicon", "display_name": "Analyst"}})).into_response()
}

async fn create(State(m): State<Mock>, Json(body): Json<Value>) -> axum::response::Response {
    if m.creates.fetch_add(1, Ordering::SeqCst) == 0 {
        let mut r = (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error": {"code": "slow_down", "message": "Too many requests.", "hint": "Wait a second."}})),
        )
            .into_response();
        r.headers_mut().insert("retry-after", "1".parse().unwrap());
        return r;
    }
    (StatusCode::CREATED, Json(json!({"id": "k3j9xq2mpa7d", "title": body["title"], "role": "owner", "version": 0}))).into_response()
}

async fn import(Query(q): Query<Vec<(String, String)>>, headers: HeaderMap, body: Bytes) -> axum::response::Response {
    let get = |k: &str| q.iter().find(|(a, _)| a == k).map(|(_, b)| b.clone()).unwrap_or_default();
    let title = format!(
        "{}|{}|{}|{}",
        get("filename"),
        body.len(),
        headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or(""),
        headers.get("content-length").and_then(|v| v.to_str().ok()).unwrap_or("")
    );
    (StatusCode::CREATED, Json(json!({"id": "k3j9xq2mpa7d", "title": title}))).into_response()
}

async fn download() -> impl IntoResponse {
    (
        [("content-type", "text/csv"), ("content-disposition", "attachment; filename=\"Q3 revenue.csv\"")],
        "region,amount\nEU,1200.5\n",
    )
}

async fn events(State(m): State<Mock>, Query(q): Query<Vec<(String, String)>>) -> axum::response::Response {
    let since = q.iter().find(|(k, _)| k == "since").map(|(_, v)| v.clone());
    m.sinces.lock().await.push(since);
    let n = m.connections.fetch_add(1, Ordering::SeqCst);
    let body = if n == 0 {
        // First connection: one change, then the connection drops.
        "event: change\ndata: {\"version\": 5, \"by\": {\"id\": \"c:ada\"}, \"at\": \"2026-10-17T08:00:00Z\", \"changes\": [{\"tab\": 1, \"range\": \"B2\"}]}\n\n".to_owned()
    } else {
        "event: ping\ndata: {}\n\nevent: change\ndata: {\"version\": 6, \"by\": {\"id\": \"si:analyst\"}, \"changes\": [{\"tab\": 1, \"reload\": true}]}\n\n"
            .to_owned()
    };
    ([("content-type", "text/event-stream")], body).into_response()
}

async fn start() -> (String, Mock) {
    let mock = Mock::default();
    let app = Router::new()
        .route("/v1/me", get(me))
        .route("/v1/sheets", post(create))
        .route("/v1/sheets/import", post(import))
        .route("/v1/sheets/{id}/download", get(download))
        .route("/v1/sheets/{id}/events", get(events))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://127.0.0.1:{port}"), mock)
}

/// Hands out "stale" until the service refuses it, then "fresh".
struct Rotating {
    current: Mutex<String>,
    refreshes: AtomicUsize,
}

impl TokenSource for Rotating {
    fn token(&self) -> BoxFuture<'_, silicon_calc_client::Result<Option<String>>> {
        Box::pin(async move { Ok(Some(self.current.lock().await.clone())) })
    }

    fn refresh<'a>(&'a self, rejected: &'a str) -> BoxFuture<'a, silicon_calc_client::Result<Option<String>>> {
        Box::pin(async move {
            let mut current = self.current.lock().await;
            if *current == rejected {
                self.refreshes.fetch_add(1, Ordering::SeqCst);
                *current = "fresh".into();
            }
            Ok(Some(current.clone()))
        })
    }
}

#[tokio::test]
async fn refreshes_once_after_a_401() {
    let (url, _) = start().await;
    let source = Arc::new(Rotating { current: Mutex::new("stale".into()), refreshes: AtomicUsize::new(0) });
    let client = Client::new(&url).unwrap().with_token_source(source.clone());
    let me = client.me().await.unwrap();
    assert_eq!(me.id, "si:analyst");
    assert_eq!(source.refreshes.load(Ordering::SeqCst), 1);
    // The fresh token is used from now on, without refreshing again.
    client.me().await.unwrap();
    assert_eq!(source.refreshes.load(Ordering::SeqCst), 1);
    // A plain token that is refused stays refused, with the service's words.
    let e = Client::new(&url).unwrap().with_token("nope").me().await.unwrap_err();
    assert!(e.is_signed_out());
    assert_eq!(e.code(), "signed_out");
    assert_eq!(e.hint(), Some("calc login"));
}

#[tokio::test]
async fn waits_out_slow_down() {
    let (url, mock) = start().await;
    let client = Client::new(&url).unwrap().with_token("fresh");
    let started = std::time::Instant::now();
    let sheet = client.create_sheet(&NewSheet { title: Some("Q3".into()), folder_id: None }).await.unwrap();
    assert_eq!(sheet.title, "Q3");
    assert_eq!(mock.creates.load(Ordering::SeqCst), 2);
    assert!(started.elapsed() >= Duration::from_millis(900));
    // With retries off, the 429 comes back typed, with its Retry-After.
    mock.creates.store(0, Ordering::SeqCst);
    let client = Client::builder().base_url(&url).retries(0).build().unwrap().with_token("fresh");
    let e = client.create_sheet(&NewSheet::default()).await.unwrap_err();
    assert_eq!(e.status(), Some(429));
    assert_eq!(e.retry_after(), Some(Duration::from_secs(1)));
}

#[tokio::test]
async fn streams_files_both_ways() {
    let (url, _) = start().await;
    let client = Client::new(&url).unwrap().with_token("fresh");
    let dir = std::env::temp_dir().join(format!("calc-client-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sales.csv");
    std::fs::write(&path, "region,amount\nEU,1200.5\nUS,900\n").unwrap();
    let upload = Upload::from_path(&path).unwrap();
    assert_eq!(upload.len(), 31);
    let sheet = client.import_sheet(&upload, &ImportSheet::default()).await.unwrap();
    assert_eq!(sheet.title, "sales.csv|31|text/csv|31");
    let d = client.download(&Line::main("k3j9xq2mpa7d"), &DownloadQuery::new(Format::Csv)).await.unwrap();
    assert_eq!(d.file_name.as_deref(), Some("Q3 revenue.csv"));
    let out = dir.join("out.csv");
    let n = d.save(&out).await.unwrap();
    assert_eq!(n, 24);
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "region,amount\nEU,1200.5\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn live_events_reconnect_from_the_last_version() {
    let (url, mock) = start().await;
    let client = Client::new(&url).unwrap().with_token("fresh");
    let mut stream = client.events(&Line::main("k3j9xq2mpa7d"), EventOptions::since(Some(3)));
    let mut seen = Vec::new();
    while let Some(event) = tokio::time::timeout(Duration::from_secs(10), stream.next()).await.unwrap() {
        let event = event.unwrap();
        seen.push(event.name().to_owned());
        if let Event::Change(c) = &event
            && c.version == 6
        {
            break;
        }
    }
    assert_eq!(seen, ["change", "reconnecting", "ping", "change"]);
    assert_eq!(stream.last_version(), Some(6));
    let sinces = mock.sinces.lock().await.clone();
    assert_eq!(sinces, [Some("3".to_owned()), Some("5".to_owned())]);
}
