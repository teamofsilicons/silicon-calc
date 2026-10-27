mod support;
use axum::http::StatusCode;
use serde_json::{Value, json};
use support::Harness;

#[tokio::test]
async fn email_gate_covers_public_content_and_owner_only_history() {
    let h = Harness::start().await;
    let owner = h.carbon("c:owner", "Owner");
    let reader = h.carbon("c:reader", "Reader");
    let no_email = h.silicon("si:reader", "Silicon reader");
    let (_, sheet) = h.post(&owner, "/v1/sheets", json!({"title":"Gated content"})).await;
    let id = sheet["id"].as_str().unwrap();
    let base = format!("/v1/sheets/{id}");
    let (_, copy) = h.post(&owner, &format!("{base}/copies"), json!({"name":"Draft"})).await;
    let copy_id = copy["id"].as_str().unwrap();
    assert_eq!(h.put(&owner, &format!("{base}/public"), json!({"enabled":true,"access":"edit","require_email":true})).await.0, StatusCode::OK);
    for suffix in ["".to_string(), "/tabs".into(), "/tabs/1/cells?range=A1".into(), "/download?format=csv".into(), "/versions".into(), "/comments".into(), "/events".into(), format!("/copies/{copy_id}/tabs")] {
        let (status, error) = h.get_anon(&format!("{base}{suffix}")).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{suffix}: {error}");
        assert_eq!(error["error"]["code"], "email_required");
    }
    let encoded = format!("/v1/sheets/%{:02x}{}", id.as_bytes()[0], &id[1..]);
    assert_eq!(h.get_anon(&encoded).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(h.get(&no_email, &base).await.1["error"]["code"], "email_required");
    let invalid = h.http.post(format!("{}{base}/visits", h.url)).json(&json!({"email":"nope"})).send().await.unwrap();
    assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let guest = h.http.post(format!("{}{base}/visits", h.url)).json(&json!({"email":" Guest@Example.com "})).send().await.unwrap();
    assert_eq!(guest.status(), StatusCode::OK);
    let body: Value = guest.json().await.unwrap();
    let grant = body["view_grant"].as_str().unwrap();
    let open = h.http.get(format!("{}{base}", h.url)).header("x-calc-view-grants", grant).send().await.unwrap();
    assert_eq!(open.status(), StatusCode::OK);
    // A receipt cannot sign in, edit, reveal viewers, or open a private file.
    let edit = h.http.post(format!("{}{base}/edits", h.url)).header("x-calc-view-grants", grant).json(&json!({"edits":[]})).send().await.unwrap();
    assert_eq!(edit.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(h.get_anon(&format!("{base}/viewers")).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(h.get(&reader, &base).await.0, StatusCode::OK);
    assert_eq!(h.get(&reader, &format!("{base}/viewers")).await.0, StatusCode::FORBIDDEN);
    assert_eq!(h.put(&reader, &format!("{base}/public"), json!({"require_email":false})).await.0, StatusCode::FORBIDDEN);
    let (_, history) = h.get(&owner, &format!("{base}/viewers")).await;
    assert_eq!(history["total"], 2);
    assert!(history["viewers"].as_array().unwrap().iter().any(|v| v["email"] == "guest@example.com" && v["email_source"] == "entered"));
    assert!(history["viewers"].as_array().unwrap().iter().any(|v| v["email"] == "reader@example.test" && v["email_source"] == "account"));
    // Repeated reads coalesce; owners aren't added, and pagination is bounded.
    h.get(&reader, &base).await;
    h.get(&owner, &base).await;
    assert_eq!(h.get(&owner, &format!("{base}/viewers")).await.1["total"], 2);
    assert_eq!(h.get(&owner, &format!("{base}/viewers?offset=50")).await.1["viewers"], json!([]));
    h.put(&owner, &format!("{base}/public"), json!({"require_email":false})).await;
    assert_eq!(h.get_anon(&base).await.0, StatusCode::OK);
    h.put(&owner, &format!("{base}/public"), json!({"require_email":true})).await;
    assert_eq!(h.http.get(format!("{}{base}", h.url)).header("x-calc-view-grants", grant).send().await.unwrap().status(), StatusCode::FORBIDDEN);
    h.put(&owner, &format!("{base}/public"), json!({"enabled":false})).await;
    assert_eq!(h.http.get(format!("{}{base}", h.url)).header("x-calc-view-grants", grant).send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
    assert_eq!(h.get(&owner, &format!("{base}/viewers")).await.1["total"], 2);
}

#[tokio::test]
async fn enabling_email_requirement_closes_existing_public_streams() {
    let h = Harness::start().await;
    let owner = h.carbon("c:owner", "Owner");
    let (_, sheet) = h.post(&owner, "/v1/sheets", json!({"title":"Live gate"})).await;
    let base = format!("/v1/sheets/{}", sheet["id"].as_str().unwrap());
    h.put(&owner, &format!("{base}/public"), json!({"enabled":true})).await;
    let mut stream = h.http.get(format!("{}{base}/events", h.url)).send().await.unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    stream.chunk().await.unwrap().unwrap();
    h.put(&owner, &format!("{base}/public"), json!({"require_email":true})).await;
    let text = tokio::time::timeout(std::time::Duration::from_secs(3), stream.text()).await.unwrap().unwrap();
    assert!(text.contains("event: access"), "{text}");
    assert!(text.contains("email_required"), "{text}");
}
