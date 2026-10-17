//! The publisher against an in-process stand-in Ting (the v1 WebSocket, with behaviour chosen per send) and an
//! in-process stand-in Silicon Accounts (App verification proofs). `tests/stack.rs` runs it against the real local
//! stack and the mock Ting.

use std::{
    collections::VecDeque,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use base64::Engine as _;
use calc_ting::{Accepted, Config, Error, Options, Publisher};
use futures::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

const SECRET: &str = "sa_app_calc_test_secret_0123456789";
const ADA: &str = "6667d4b4-1f0b-4c8e-9a52-0d0d6f8c2a11";

// ------------------------------------------------------------------------------------- stand-in Silicon Accounts

#[derive(Clone, Default)]
struct Accounts {
    issued: Arc<AtomicUsize>,
    ttl_seconds: Arc<AtomicU64>,
    unavailable: Arc<AtomicBool>,
    bodies: Arc<Mutex<Vec<Value>>>,
}

async fn issue(State(s): State<Accounts>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let expected = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("calc:{SECRET}")));
    if headers.get("authorization").and_then(|h| h.to_str().ok()) != Some(expected.as_str()) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": {"code": "invalid_client", "message": "The app credentials are wrong.", "hint": "Check the secret."}}))).into_response();
    }
    if s.unavailable.load(Ordering::SeqCst) {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": {"code": "unavailable", "message": "Down for a moment."}}))).into_response();
    }
    s.bodies.lock().unwrap().push(body.clone());
    let n = s.issued.fetch_add(1, Ordering::SeqCst) + 1;
    let ttl = match s.ttl_seconds.load(Ordering::SeqCst) {
        0 => 1800,
        t => t,
    };
    let expires = (OffsetDateTime::now_utc() + time::Duration::seconds(ttl as i64)).format(&Rfc3339).unwrap();
    (
        StatusCode::CREATED,
        Json(json!({
            "proof_id": format!("prf_{n}"), "kind": "app_verification", "proof_token": format!("sap_test_{n}"),
            "expires_at": expires, "proof_refresh_token": format!("sapr_test_{n}"), "refresh_expires_at": expires,
            "issuing_app": "calc", "receiving_app": body["receiving_app"], "user": null, "scopes": body["scopes"],
        })),
    )
        .into_response()
}

async fn start_accounts() -> (String, Accounts) {
    let state = Accounts::default();
    let app = Router::new().route("/v1/proofs/app-verification", post(issue)).with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), state)
}

// ------------------------------------------------------------------------------------------------ stand-in Ting

#[derive(Clone, Debug)]
enum Behave {
    Accept,
    Refuse(&'static str),
    /// Close the TCP connection without answering.
    Drop,
    /// Never answer.
    Silent,
    /// Keep this send unanswered until the next one arrives, then answer the next one first.
    Swap,
}

#[derive(Clone, Default)]
struct Ting {
    frames: Arc<Mutex<Vec<Value>>>,
    connections: Arc<AtomicUsize>,
    plan: Arc<Mutex<VecDeque<Behave>>>,
    bad_ready: Arc<AtomicBool>,
    accepted: Arc<AtomicUsize>,
}

impl Ting {
    fn plan(&self, steps: &[Behave]) {
        self.plan.lock().unwrap().extend(steps.iter().cloned());
    }

    fn frames(&self) -> Vec<Value> {
        self.frames.lock().unwrap().clone()
    }

    fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

fn acceptance(t: &Ting, frame: &Value) -> Value {
    let body: Value = serde_json::from_str(frame["body"].as_str().unwrap()).unwrap();
    let n = t.accepted.fetch_add(1, Ordering::SeqCst) + 1;
    json!({"op": "accepted", "request_id": frame["request_id"], "id": format!("ting_{n}"),
           "created_at": OffsetDateTime::now_utc().format(&Rfc3339).unwrap(), "status": "accepted",
           "key": body["key"], "silent": false})
}

async fn serve_ting(t: Ting, stream: tokio::net::TcpStream) {
    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else { return };
    t.connections.fetch_add(1, Ordering::SeqCst);
    let ready = if t.bad_ready.load(Ordering::SeqCst) { json!({"op": "ready", "protocol": "v0"}) } else { json!({"op": "ready", "receiver_id": "recv_test", "protocol": "v1"}) };
    if ws.send(Message::Text(ready.to_string().into())).await.is_err() {
        return;
    }
    let mut held: Option<Value> = None;
    while let Some(Ok(msg)) = ws.next().await {
        let Message::Text(text) = msg else { continue };
        let frame: Value = serde_json::from_str(&text).unwrap();
        t.frames.lock().unwrap().push(frame.clone());
        let step = t.plan.lock().unwrap().pop_front().unwrap_or(Behave::Accept);
        match step {
            Behave::Accept => {
                let reply = acceptance(&t, &frame);
                if ws.send(Message::Text(reply.to_string().into())).await.is_err() {
                    return;
                }
                if let Some(first) = held.take() {
                    let reply = acceptance(&t, &first);
                    if ws.send(Message::Text(reply.to_string().into())).await.is_err() {
                        return;
                    }
                }
            }
            Behave::Refuse(code) => {
                let reply = json!({"op": "error", "request_id": frame["request_id"], "error": {"code": code, "message": format!("refused: {code}")}});
                if ws.send(Message::Text(reply.to_string().into())).await.is_err() {
                    return;
                }
            }
            Behave::Drop => return,
            Behave::Silent => {}
            Behave::Swap => held = Some(frame),
        }
    }
}

async fn start_ting() -> (String, Ting) {
    let t = Ting::default();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let state = t.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(serve_ting(state.clone(), stream));
        }
    });
    (format!("http://{addr}"), t)
}

// ------------------------------------------------------------------------------------------------------ helpers

fn config(ting_url: &str, accounts_url: &str) -> Config {
    Config {
        ting_url: ting_url.into(),
        accounts_api_url: accounts_url.into(),
        app_id: "calc".into(),
        app_secret: SECRET.into(),
        ting_app_id: "ting".into(),
        allow_http: true,
    }
}

fn fast() -> Options {
    Options { request_timeout: Duration::from_millis(1500), connect_timeout: Duration::from_millis(1000), ..Options::default() }
}

fn ting(key: &str) -> Value {
    json!({"type": "calc.mention.received", "for": ADA, "key": key,
           "data": {"sheet": {"id": "k3j9xq2mpa7d", "title": "Q3 revenue"}, "range": "B2:B9", "comment": {"body": "@si:analyst why?"}},
           "metadata": {}})
}

async fn setup(options: Options) -> (Publisher, Ting, Accounts) {
    let (accounts_url, accounts) = start_accounts().await;
    let (ting_url, t) = start_ting().await;
    (Publisher::with_options(config(&ting_url, &accounts_url), options).unwrap(), t, accounts)
}

// -------------------------------------------------------------------------------------------------------- tests

#[tokio::test]
async fn sends_reuse_one_connection_and_one_proof() {
    let (p, t, accounts) = setup(fast()).await;
    let mut ids = Vec::new();
    for i in 0..3 {
        let a: Accepted = p.send(&ting(&format!("k{i}"))).await.expect("accepted");
        assert!(a.id.starts_with("ting_"));
        assert!(!a.silent);
        assert!(OffsetDateTime::parse(&a.created_at, &Rfc3339).is_ok());
        ids.push(a.id);
    }
    ids.dedup();
    assert_eq!(ids.len(), 3);
    assert_eq!(t.connections(), 1, "one WebSocket for every send");
    assert_eq!(accounts.issued.load(Ordering::SeqCst), 1, "one proof for every send");
    let body = accounts.bodies.lock().unwrap()[0].clone();
    assert_eq!(body["receiving_app"], "ting");
    assert_eq!(body["scopes"], json!(["tings.send"]));
}

#[tokio::test]
async fn a_send_carries_the_exact_ting_text_and_the_proof() {
    let (p, t, _) = setup(fast()).await;
    let body = ting("exact");
    p.send(&body).await.unwrap();
    let frame = &t.frames()[0];
    let fields: Vec<&str> = frame.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(fields.len(), 4, "{frame}");
    assert_eq!(frame["op"], "send");
    assert!(frame["request_id"].as_str().is_some_and(|r| !r.is_empty()));
    assert_eq!(frame["proof_token"], "sap_test_1");
    let text = frame["body"].as_str().expect("the body is the JSON text");
    assert_eq!(text, serde_json::to_string(&body).unwrap());
}

#[tokio::test]
async fn many_sends_at_once_share_the_connection_and_the_proof() {
    let (p, t, accounts) = setup(fast()).await;
    let p = Arc::new(p);
    let mut handles = Vec::new();
    for i in 0..20 {
        let p = p.clone();
        handles.push(tokio::spawn(async move { p.send(&ting(&format!("burst-{i}"))).await }));
    }
    for h in handles {
        h.await.unwrap().expect("accepted");
    }
    assert_eq!(t.connections(), 1);
    assert_eq!(accounts.issued.load(Ordering::SeqCst), 1, "concurrent sends wait for the one proof being issued");
    assert_eq!(t.frames().len(), 20);
}

#[tokio::test]
async fn answers_are_matched_to_sends_by_request_id() {
    let (p, t, _) = setup(fast()).await;
    // The first send is answered only after the second one, which is answered first.
    t.plan(&[Behave::Swap, Behave::Accept]);
    let p = Arc::new(p);
    let first = {
        let p = p.clone();
        tokio::spawn(async move { p.send(&ting("first")).await })
    };
    while t.frames().is_empty() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let second = p.send(&ting("second")).await.unwrap();
    let first = first.await.unwrap().unwrap();
    // ting_1 went to the second send (answered first), ting_2 to the first.
    assert_eq!(second.id, "ting_1");
    assert_eq!(first.id, "ting_2");
}

#[tokio::test]
async fn a_refused_proof_is_replaced_and_the_ting_sent_again() {
    let (p, t, accounts) = setup(fast()).await;
    t.plan(&[Behave::Refuse("invalid_proof"), Behave::Accept]);
    p.send(&ting("again")).await.expect("accepted with a new proof");
    assert_eq!(accounts.issued.load(Ordering::SeqCst), 2);
    let frames = t.frames();
    assert_eq!(frames[0]["proof_token"], "sap_test_1");
    assert_eq!(frames[1]["proof_token"], "sap_test_2");
    assert_eq!(frames[0]["body"], frames[1]["body"], "the same ting, byte for byte");
    // The new proof is the one kept.
    p.send(&ting("after")).await.unwrap();
    assert_eq!(t.frames()[2]["proof_token"], "sap_test_2");
    assert_eq!(accounts.issued.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_proof_refused_twice_is_reported() {
    let (p, t, accounts) = setup(fast()).await;
    t.plan(&[Behave::Refuse("invalid_proof"), Behave::Refuse("invalid_proof")]);
    let e = p.send(&ting("never")).await.unwrap_err();
    assert!(matches!(&e, Error::Rejected { code } if code == "invalid_proof"), "{e:?}");
    assert!(e.retryable());
    assert_eq!(accounts.issued.load(Ordering::SeqCst), 2);
    // And the next send starts with a fresh proof.
    p.send(&ting("later")).await.unwrap();
    assert_eq!(accounts.issued.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn a_proof_is_replaced_before_it_expires() {
    let (p, t, accounts) = setup(fast()).await;
    accounts.ttl_seconds.store(2, Ordering::SeqCst);
    p.send(&ting("one")).await.unwrap();
    p.send(&ting("two")).await.unwrap();
    assert_eq!(accounts.issued.load(Ordering::SeqCst), 1, "still fresh");
    tokio::time::sleep(Duration::from_millis(1200)).await;
    p.send(&ting("three")).await.unwrap();
    assert_eq!(accounts.issued.load(Ordering::SeqCst), 2, "replaced half way through its two seconds");
    assert_eq!(t.frames()[2]["proof_token"], "sap_test_2");
}

#[tokio::test]
async fn refusals_say_whether_to_try_again() {
    let (p, t, _) = setup(fast()).await;
    t.plan(&[Behave::Refuse("unknown_recipient"), Behave::Refuse("invalid_ting"), Behave::Refuse("unavailable"), Behave::Refuse("idempotency_conflict")]);
    let e = p.send(&ting("a")).await.unwrap_err();
    assert!(matches!(&e, Error::Rejected { code } if code == "unknown_recipient") && !e.retryable(), "{e:?}");
    let e = p.send(&ting("b")).await.unwrap_err();
    assert!(matches!(&e, Error::Rejected { code } if code == "invalid_ting") && !e.retryable(), "{e:?}");
    let e = p.send(&ting("c")).await.unwrap_err();
    assert!(matches!(&e, Error::Rejected { code } if code == "unavailable") && e.retryable(), "{e:?}");
    let e = p.send(&ting("d")).await.unwrap_err();
    assert!(matches!(&e, Error::Rejected { code } if code == "idempotency_conflict") && !e.retryable(), "{e:?}");
    // Refusals don't cost the connection.
    p.send(&ting("e")).await.unwrap();
    assert_eq!(t.connections(), 1);
}

#[tokio::test]
async fn a_dropped_connection_fails_its_send_and_the_next_send_reconnects() {
    let (p, t, _) = setup(fast()).await;
    p.send(&ting("before")).await.unwrap();
    t.plan(&[Behave::Drop]);
    let e = p.send(&ting("dropped")).await.unwrap_err();
    assert!(matches!(e, Error::Transport(_)), "{e:?}");
    assert!(e.retryable());
    let started = Instant::now();
    let a = p.send(&ting("dropped")).await.expect("the same ting goes through on a new connection");
    assert!(a.id.starts_with("ting_"));
    assert_eq!(t.connections(), 2);
    assert!(started.elapsed() < Duration::from_millis(1200), "a connection that worked is opened again quickly");
}

#[tokio::test]
async fn an_unanswered_send_times_out_and_the_connection_is_replaced() {
    let options = Options { request_timeout: Duration::from_millis(600), ..fast() };
    let (p, t, _) = setup(options).await;
    p.send(&ting("answered")).await.unwrap();
    t.plan(&[Behave::Silent]);
    let started = Instant::now();
    let e = p.send(&ting("silent")).await.unwrap_err();
    assert!(matches!(e, Error::Transport(_)), "{e:?}");
    assert!(e.retryable());
    assert!(started.elapsed() >= Duration::from_millis(550) && started.elapsed() < Duration::from_millis(1600), "{:?}", started.elapsed());
    p.send(&ting("next")).await.unwrap();
    assert_eq!(t.connections(), 2, "the connection with the unanswered send is not reused");
}

#[tokio::test]
async fn a_ting_that_answers_nothing_is_backed_off_from() {
    let options = Options { request_timeout: Duration::from_millis(400), ..fast() };
    let (p, t, _) = setup(options).await;
    // A first connection that never answers counts as a failure: the next send can't wait a second for the next
    // attempt within its 400 ms, so it fails at once instead of piling onto a broken Ting.
    t.plan(&[Behave::Silent]);
    assert!(matches!(p.send(&ting("silent")).await.unwrap_err(), Error::Transport(_)));
    let started = Instant::now();
    let e = p.send(&ting("next")).await.unwrap_err();
    assert!(matches!(&e, Error::Transport(m) if m.contains("next attempt")), "{e:?}");
    assert!(started.elapsed() < Duration::from_millis(100), "{:?}", started.elapsed());
    assert_eq!(t.connections(), 1);
}

#[tokio::test]
async fn an_unreachable_ting_is_a_transport_error_and_retries_back_off() {
    let (accounts_url, _) = start_accounts().await;
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let p = Publisher::with_options(config(&format!("http://127.0.0.1:{port}"), &accounts_url), fast()).unwrap();
    let e = p.send(&ting("nobody")).await.unwrap_err();
    assert!(matches!(e, Error::Transport(_)), "{e:?}");
    assert!(e.retryable());
    // Right after a failed attempt, the next send waits for the backoff (about a second) and tries once more.
    let started = Instant::now();
    let e = p.send(&ting("nobody")).await.unwrap_err();
    assert!(matches!(e, Error::Transport(_)), "{e:?}");
    let waited = started.elapsed();
    assert!(waited >= Duration::from_millis(500) && waited < Duration::from_millis(1600), "{waited:?}");
}

#[tokio::test]
async fn proofs_that_cant_be_issued_are_authority_errors() {
    let (ting_url, t) = start_ting().await;
    let (accounts_url, accounts) = start_accounts().await;
    let wrong = Publisher::with_options(Config { app_secret: "sa_app_calc_wrong_000000000000".into(), ..config(&ting_url, &accounts_url) }, fast()).unwrap();
    let e = wrong.send(&ting("x")).await.unwrap_err();
    assert!(matches!(&e, Error::Authority(m) if m.contains("credentials") || m.contains("invalid_client")), "{e:?}");
    assert!(e.retryable());
    accounts.unavailable.store(true, Ordering::SeqCst);
    let p = Publisher::with_options(config(&ting_url, &accounts_url), fast()).unwrap();
    assert!(matches!(p.send(&ting("y")).await.unwrap_err(), Error::Authority(_)));
    assert_eq!(t.connections(), 0, "nothing is sent without a proof");
    accounts.unavailable.store(false, Ordering::SeqCst);
    p.send(&ting("y")).await.unwrap();
}

#[tokio::test]
async fn a_wrong_greeting_is_a_protocol_error() {
    let (p, t, _) = setup(fast()).await;
    t.bad_ready.store(true, Ordering::SeqCst);
    let e = p.send(&ting("hello")).await.unwrap_err();
    assert!(matches!(e, Error::Protocol(_)), "{e:?}");
    assert!(e.retryable());
}

#[tokio::test]
async fn a_ting_that_can_never_be_accepted_is_refused_before_anything_is_sent() {
    let (p, t, accounts) = setup(fast()).await;
    let mut bad = ting("x");
    bad["for"] = json!("c:ada");
    let e = p.send(&bad).await.unwrap_err();
    assert!(matches!(e, Error::Invalid(_)) && !e.retryable(), "{e:?}");
    assert!(matches!(p.send(&json!({"type": "calc.sheet.shared"})).await.unwrap_err(), Error::Invalid(_)));
    assert_eq!(t.connections(), 0);
    assert_eq!(accounts.issued.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_idle_connection_is_closed_and_opened_again_for_the_next_send() {
    let options = Options { idle_timeout: Duration::from_millis(200), ..fast() };
    let (p, t, _) = setup(options).await;
    p.send(&ting("one")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(450)).await;
    p.send(&ting("two")).await.unwrap();
    assert_eq!(t.connections(), 2);
}

#[test]
fn a_publisher_outlives_the_runtime_that_first_used_it() {
    let (accounts_url, ting_url) = {
        // The stand-ins live on their own runtime thread for the whole test.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async move {
                let (a, _) = start_accounts().await;
                let (t, _) = start_ting().await;
                tx.send((a, t)).unwrap();
                std::future::pending::<()>().await;
            });
        });
        rx.recv().unwrap()
    };
    let p = Publisher::with_options(config(&ting_url, &accounts_url), fast()).unwrap();
    let first = tokio::runtime::Runtime::new().unwrap();
    first.block_on(p.send(&ting("on the first runtime"))).unwrap();
    drop(first);
    let second = tokio::runtime::Runtime::new().unwrap();
    second.block_on(p.send(&ting("on the second runtime"))).expect("a new connection task is started");
}
