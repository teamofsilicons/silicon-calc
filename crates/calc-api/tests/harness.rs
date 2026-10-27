//! The harness itself works: a token from the stand-in Accounts is accepted.

mod support;

#[test]
fn native_peer_database_url_parses() {
    let options: sqlx::postgres::PgConnectOptions =
        "postgresql:///silicon_calc?host=/var/run/postgresql&user=calc".parse().unwrap();
    assert_eq!(options.get_username(), "calc");
    assert_eq!(options.get_database(), Some("silicon_calc"));
    assert_eq!(options.get_socket().unwrap().to_str(), Some("/var/run/postgresql"));
}

#[tokio::test]
async fn health_and_sign_in() {
    let h = support::Harness::start().await;
    let r = h.http.get(format!("{}/healthz", h.url)).send().await.expect("request");
    assert_eq!(r.status().as_u16(), 200);
    let r = h.http.get(format!("{}/readyz", h.url)).send().await.expect("readiness request");
    assert_eq!(r.status().as_u16(), 204);
    let ada = h.carbon("c:ada", "Ada");
    let token = h.state.accounts.verify(&ada.token).await.expect("token verifies");
    assert_eq!(token.uuid, ada.uuid);
    assert_eq!(token.kind, "carbon");
    let looked_up = h.state.accounts.by_public_id("c:ada").await.expect("lookup").expect("found");
    assert_eq!(looked_up.display_name, "Ada");
    h.state.db.close().await;
    let r = h.http.get(format!("{}/readyz", h.url)).send().await.expect("unready request");
    assert_eq!(r.status().as_u16(), 503);
}

#[cfg(unix)]
#[tokio::test]
async fn service_drains_on_sigterm() {
    use std::{process::Stdio, time::Duration};
    let h = support::Harness::start().await;
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let cfg = &h.state.cfg;
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_calc-api"))
        .env("CALC_BIND", format!("127.0.0.1:{port}"))
        .env("CALC_DATABASE_URL", &cfg.database_url)
        .env("CALC_ACCOUNTS_URL", &cfg.accounts_url)
        .env("CALC_ACCOUNTS_API_URL", &cfg.accounts_api_url)
        .env("CALC_APP_SECRET", &cfg.app_secret)
        .env("CALC_BLOB_DIR", cfg.blob_dir.join("signal-probe"))
        .env("CALC_ALLOW_HTTP_ACCOUNTS", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn().expect("spawn service");
    let url = format!("http://127.0.0.1:{port}/readyz");
    let mut ready = false;
    for _ in 0..100 {
        if h.http.get(&url).send().await.is_ok_and(|r| r.status().as_u16() == 204) {
            ready = true;
            break;
        }
        assert!(child.try_wait().unwrap().is_none(), "service exited before readiness");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(ready, "service did not become ready");
    let pid = child.id().expect("owned child pid").to_string();
    let sent = tokio::process::Command::new("kill").args(["-TERM", &pid]).status().await.unwrap();
    assert!(sent.success());
    let output = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output()).await
        .expect("service did not drain").unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("shutting down: saving open lines"));
}
