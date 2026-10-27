mod support;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Ting {
    calls: Arc<Mutex<Vec<(String, Value)>>>,
    grants: Arc<Mutex<Vec<Value>>>,
    fail: Arc<Mutex<bool>>,
}

impl Ting {
    async fn start(&self) -> String {
        let router = Router::new()
            .route("/v1/{*path}", post(call))
            .with_state(self.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        format!("http://{addr}")
    }
}

async fn call(
    State(s): State<Ting>,
    Path(path): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    s.calls.lock().unwrap().push((path.clone(), body.clone()));
    if *s.fail.lock().unwrap() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"private":"upstream-private-value"})),
        )
            .into_response();
    }
    let scope = match path.as_str() {
        "subscriptions/query" => "subscriptions.read",
        "subscriptions" => "subscriptions.register",
        "subscriptions/revoke" => "subscriptions.revoke",
        _ => panic!("unexpected mutation {path}"),
    };
    assert_eq!(headers["authorization"], format!("Bearer sap_test_{scope}"));
    assert_eq!(body["app_id"], "calc");
    let mut grants = s.grants.lock().unwrap();
    let answer = match path.as_str() {
        "subscriptions/query" => {
            json!({"items":grants.iter().filter(|g| g["for"] == body["for"]).cloned().collect::<Vec<_>>()})
        }
        "subscriptions" => {
            let grant = json!({"id":"sub_test","app_id":"calc","for":body["for"],"active":true,"required_delivery":false});
            grants.retain(|g| g["for"] != body["for"]);
            grants.push(grant.clone());
            grant
        }
        _ => {
            let grant = grants.iter_mut().find(|g| g["id"] == body["id"]).unwrap();
            grant["active"] = json!(false);
            json!({"id":grant["id"],"active":false,"required_delivery":false})
        }
    };
    Json(answer).into_response()
}

#[tokio::test]
async fn notifications_require_explicit_consent_and_use_only_the_verified_recipient() {
    let ting = Ting::default();
    let h = support::Harness::start_with_ting(Some(ting.start().await)).await;
    let ada = h.carbon("c:ada", "Ada");
    let bob = h.carbon("c:bob", "Bob");
    let path = "/v1/me/notifications";
    assert_eq!(h.get_anon(path).await.0, StatusCode::UNAUTHORIZED);
    assert!(ting.calls.lock().unwrap().is_empty());

    assert_eq!(
        h.get(&ada, path).await,
        (StatusCode::OK, json!({"enabled":false}))
    );
    assert!(
        h.accounts
            .proof_requests()
            .iter()
            .all(|p| p["kind"] == "app-verification")
    );
    assert!(
        ting.calls
            .lock()
            .unwrap()
            .iter()
            .all(|(p, _)| p == "subscriptions/query")
    );
    assert!(
        h.post(&ada, path, json!({"for":bob.uuid}))
            .await
            .0
            .is_client_error()
    );
    assert!(
        !h.accounts
            .proof_requests()
            .iter()
            .any(|p| p["kind"] == "user-verification")
    );

    assert_eq!(
        h.post(&ada, path, json!({})).await,
        (StatusCode::OK, json!({"enabled":true}))
    );
    let proofs = h.accounts.proof_requests();
    let delegated = proofs
        .iter()
        .find(|p| p["kind"] == "user-verification")
        .unwrap();
    assert_eq!(delegated["body"]["subject_token"], ada.token);
    assert_eq!(
        delegated["body"]["scopes"],
        json!(["subscriptions.register"])
    );
    assert_eq!(delegated["body"]["receiving_app"], "ting");
    assert_eq!(h.get(&ada, path).await.1, json!({"enabled":true}));
    assert_eq!(h.get(&bob, path).await.1, json!({"enabled":false}));

    // Another account cannot revoke Ada's grant, even by supplying its id in the URL.
    let response = h
        .http
        .delete(format!("{}{path}?id=sub_test", h.url))
        .bearer_auth(&bob.token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(h.get(&ada, path).await.1, json!({"enabled":true}));
    let response = h
        .http
        .delete(format!("{}{path}", h.url))
        .bearer_auth(&ada.token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({"enabled":false})
    );
    assert_eq!(h.get(&ada, path).await.1, json!({"enabled":false}));
    assert!(
        ting.calls
            .lock()
            .unwrap()
            .iter()
            .all(|(_, b)| b.get("required_delivery").is_none())
    );
}

#[tokio::test]
async fn notification_status_and_enable_fail_closed_when_ting_is_unavailable() {
    let ting = Ting::default();
    let h = support::Harness::start_with_ting(Some(ting.start().await)).await;
    let ada = h.carbon("c:ada", "Ada");
    *ting.fail.lock().unwrap() = true;
    for (status, body) in [
        h.get(&ada, "/v1/me/notifications").await,
        h.post(&ada, "/v1/me/notifications", json!({})).await,
    ] {
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["code"], "unavailable");
        assert!(body.get("enabled").is_none());
        assert!(!body.to_string().contains("upstream-private-value"));
    }
    let unconfigured = support::Harness::start().await;
    let ada = unconfigured.carbon("c:ada", "Ada");
    assert_eq!(
        unconfigured.get(&ada, "/v1/me/notifications").await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(unconfigured.accounts.proof_requests().is_empty());
}
