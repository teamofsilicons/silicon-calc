//! The harness itself works: a token from the stand-in Accounts is accepted.

mod support;

#[tokio::test]
async fn health_and_sign_in() {
    let h = support::Harness::start().await;
    let r = h.http.get(format!("{}/healthz", h.url)).send().await.expect("request");
    assert_eq!(r.status().as_u16(), 200);
    let ada = h.carbon("c:ada", "Ada");
    let token = h.state.accounts.verify(&ada.token).await.expect("token verifies");
    assert_eq!(token.uuid, ada.uuid);
    assert_eq!(token.kind, "carbon");
    let looked_up = h.state.accounts.by_public_id("c:ada").await.expect("lookup").expect("found");
    assert_eq!(looked_up.display_name, "Ada");
}
