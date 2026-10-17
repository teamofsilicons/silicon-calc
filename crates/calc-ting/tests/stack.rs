//! The publisher against the real local stack: Silicon Accounts with Calc's apps (e2e/stack.json) issues and verifies
//! the proofs, and the mock Ting (e2e/mock-ting/server.ts, started here on a free port) checks every send the way
//! Ting does. They need the stack and Node:
//!
//!   scripts/accounts-stack.sh start
//!   cargo test -p calc-ting --test stack -- --ignored        (scripts/e2e.sh runs them)

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use calc_ting::{Config, Error, Options, Publisher};
use serde_json::{Value, json};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repository root")
}

fn stack() -> Value {
    let path = std::env::var("STACK_JSON").map(PathBuf::from).unwrap_or_else(|_| repo().join("e2e/stack.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} can't be read ({e}): start the stack with scripts/accounts-stack.sh start", path.display()));
    serde_json::from_str(&text).expect("stack.json is JSON")
}

/// The mock Ting on a free port, stopped when dropped.
struct MockTing {
    child: Child,
    url: String,
    http: reqwest::Client,
}

impl MockTing {
    fn start(stack_file: &Path) -> Self {
        let dir = std::env::temp_dir().join(format!("calc-ting-stack-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let ready = dir.join("ready.json");
        let child = Command::new(std::env::var("NODE").unwrap_or_else(|_| "node".into()))
            .arg(repo().join("e2e/mock-ting/server.ts"))
            .args(["--port", "0", "--ping-seconds", "5", "--stack"])
            .arg(stack_file)
            .arg("--ready-file")
            .arg(&ready)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("node runs the mock Ting");
        let started = Instant::now();
        let url = loop {
            if let Ok(text) = std::fs::read_to_string(&ready)
                && let Ok(v) = serde_json::from_str::<Value>(&text)
            {
                break v["url"].as_str().unwrap().to_owned();
            }
            assert!(started.elapsed() < Duration::from_secs(20), "the mock Ting did not start");
            std::thread::sleep(Duration::from_millis(50));
        };
        Self { child, url, http: reqwest::Client::new() }
    }

    async fn get(&self, path: &str) -> Value {
        self.http.get(format!("{}{path}", self.url)).send().await.unwrap().json().await.unwrap()
    }

    async fn post(&self, path: &str, body: Value) -> Value {
        self.http.post(format!("{}{path}", self.url)).json(&body).send().await.unwrap().json().await.unwrap()
    }
}

impl Drop for MockTing {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Setup {
    ting: MockTing,
    publisher: Publisher,
    recipient: String,
}

async fn setup() -> Setup {
    let s = stack();
    let stack_file = std::env::var("STACK_JSON").map(PathBuf::from).unwrap_or_else(|_| repo().join("e2e/stack.json"));
    let ting = MockTing::start(&stack_file);
    let api = s["accounts"]["api_url"].as_str().unwrap().to_owned();
    let secret = s["apps"]["calc"]["app_secret"].as_str().unwrap().to_owned();
    // calc's owner on the stack is a real account to send tings to.
    let client = silicon_accounts_client::AccountsClient::builder().base_url(&api).build().unwrap();
    let owner = client.as_app("calc", secret.clone()).lookup_by_id("c:calcdev").await.expect("c:calcdev exists on the stack");
    let publisher = Publisher::with_options(
        Config { ting_url: ting.url.clone(), accounts_api_url: api, app_id: "calc".into(), app_secret: secret, ting_app_id: "ting".into(), allow_http: true },
        Options { request_timeout: Duration::from_secs(5), ..Options::default() },
    )
    .unwrap();
    Setup { ting, publisher, recipient: owner.uuid }
}

fn mention(recipient: &str, key: &str) -> Value {
    json!({"type": "calc.mention.received", "for": recipient, "key": key,
           "data": {"sheet": {"id": "k3j9xq2mpa7d", "title": "Q3 revenue"}, "copy": null, "tab": {"id": 1, "name": "Sales"},
                    "range": "B2:B9", "quote": "1,200 | 900", "thread_id": "8c3e…",
                    "comment": {"id": "c1", "body": "@si:analyst why did March drop?", "author": {"id": "c:ada"}},
                    "url": "http://127.0.0.1:8731/s/k3j9xq2mpa7d", "cli": "calc comments show 8c3e"},
           "metadata": {}})
}

#[tokio::test]
#[ignore = "needs the local stack: scripts/accounts-stack.sh start"]
async fn the_stack_takes_calcs_tings_once_per_key() {
    let s = setup().await;
    let key = format!("stack-{}", uuid::Uuid::new_v4().simple());
    let first = s.publisher.send(&mention(&s.recipient, &key)).await.expect("Ting accepts Calc's ting");
    assert!(first.id.starts_with("ting_"));
    let again = s.publisher.send(&mention(&s.recipient, &key)).await.expect("the same ting again");
    assert_eq!(again, first, "the same key answers the first acceptance");
    let got = s.ting.get(&format!("/tings?for={}&type=calc.mention.received", s.recipient)).await;
    let item = got["items"].as_array().unwrap().iter().find(|t| t["key"] == key.as_str()).expect("the mock Ting recorded it");
    assert_eq!(item["id"], first.id.as_str());
    assert_eq!(item["issuing_app"], "calc");
    assert_eq!(item["data"]["comment"]["body"], "@si:analyst why did March drop?");
    assert_eq!(item["duplicates"], 1);
}

#[tokio::test]
#[ignore = "needs the local stack: scripts/accounts-stack.sh start"]
async fn the_stack_refuses_what_ting_refuses() {
    let s = setup().await;
    let nobody = uuid::Uuid::new_v4().to_string();
    let e = s.publisher.send(&mention(&nobody, "to-nobody")).await.unwrap_err();
    assert!(matches!(&e, Error::Rejected { code } if code == "unknown_recipient"), "{e:?}");
    assert!(!e.retryable());
    let mut odd = mention(&s.recipient, "odd-type");
    odd["type"] = json!("calc.sheet.exploded");
    let e = s.publisher.send(&odd).await.unwrap_err();
    assert!(matches!(&e, Error::Rejected { code } if code == "invalid_ting"), "{e:?}");
    assert!(!e.retryable());
    let key = format!("conflict-{}", uuid::Uuid::new_v4().simple());
    s.publisher.send(&mention(&s.recipient, &key)).await.unwrap();
    let mut changed = mention(&s.recipient, &key);
    changed["data"]["range"] = json!("C2:C9");
    let e = s.publisher.send(&changed).await.unwrap_err();
    assert!(matches!(&e, Error::Rejected { code } if code == "idempotency_conflict"), "{e:?}");
}

#[tokio::test]
#[ignore = "needs the local stack: scripts/accounts-stack.sh start"]
async fn the_stack_survives_drops_disconnects_and_a_refused_proof() {
    let s = setup().await;
    s.publisher.send(&mention(&s.recipient, &format!("warm-{}", uuid::Uuid::new_v4().simple()))).await.unwrap();

    // Ting drops the connection in the middle of a send: uncertain, so the same ting goes again.
    s.ting.post("/_faults", json!({"kind": "drop"})).await;
    let key = format!("dropped-{}", uuid::Uuid::new_v4().simple());
    let e = s.publisher.send(&mention(&s.recipient, &key)).await.unwrap_err();
    assert!(matches!(e, Error::Transport(_)) && e.retryable(), "{e:?}");
    s.publisher.send(&mention(&s.recipient, &key)).await.expect("accepted on a new connection");

    // Ting closes every connection between sends.
    let closed = s.ting.post("/_disconnect", json!({})).await;
    assert_eq!(closed["closed"], 1);
    tokio::time::sleep(Duration::from_millis(100)).await;
    s.publisher.send(&mention(&s.recipient, &format!("after-close-{}", uuid::Uuid::new_v4().simple()))).await.expect("reconnects");

    // Ting refuses the proof once: a new one is issued and the ting goes through.
    s.ting.post("/_faults", json!({"kind": "error", "code": "invalid_proof"})).await;
    let key = format!("new-proof-{}", uuid::Uuid::new_v4().simple());
    s.publisher.send(&mention(&s.recipient, &key)).await.expect("accepted with a new proof");
    let requests = s.ting.get("/_requests?limit=3").await;
    let items = requests["items"].as_array().unwrap();
    assert_eq!(items[0]["outcome"], "accepted");
    assert_eq!(items[0]["key"], key.as_str());
    assert_eq!(items[1]["code"], "invalid_proof");
    let connections = s.ting.get("/_connections").await;
    assert_eq!(connections["count"], 1, "one connection open at the end");
}
