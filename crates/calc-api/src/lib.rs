//! The Silicon Calc service, as a library so tests can run it in-process. `main.rs` only starts it.

pub mod accounts;
pub mod auth;
pub mod blobs;
pub mod config;
pub mod db;
pub mod error;
pub mod ids;
pub mod perms;
pub mod state;

// Platform: sheets, folders, sharing, transfers, search.
pub mod folders;
pub mod formatting;
mod javascript;
pub mod search;
pub mod sharing;
pub mod visitors;
pub mod sheets;
pub mod transfers;

// Lines: live editing, history, copies, files, code, comments, notifications.
pub mod code;
pub mod comments;
pub mod files;
pub mod imports;
pub mod lines;
pub mod notify;
pub mod notify_preferences;

use std::sync::Arc;

use axum::{Router, routing::get};
use tower_http::{limit::RequestBodyLimitLayer, trace::TraceLayer};

use crate::{accounts::Accounts, blobs::Blobs, config::Config, state::AppState};

/// Connects to everything and builds the shared state.
pub async fn build_state(cfg: Config) -> anyhow::Result<AppState> {
    let db = db::connect(&cfg.database_url).await?;
    let accounts = Accounts::new(&cfg.accounts_api_url, &cfg.accounts_url, &cfg.app_id, &cfg.app_secret, cfg.allow_http)?;
    let blobs = Blobs::new(cfg.blob_dir.clone())?;
    Ok(AppState { cfg: Arc::new(cfg), db, accounts, blobs, lines: lines::Registry::new(), notify: notify::Notifier::new() })
}

/// Starts the background tasks (notification sender, line maintenance).
pub fn start_background(state: &AppState) {
    notify::spawn_worker(state.clone());
    imports::spawn_worker(state.clone());
    lines::spawn_maintenance(state.clone());
}

/// Runs the service until SIGINT or SIGTERM, then saves every open line.
pub async fn serve(cfg: Config) -> anyhow::Result<()> {
    let bind = cfg.bind;
    let state = build_state(cfg).await?;
    start_background(&state);
    let app = router(state.clone());
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!("calc-api listening on http://{bind} (Silicon Accounts: {})", state.cfg.accounts_url);
    axum::serve(listener, app).with_graceful_shutdown(shutdown(state)).await?;
    Ok(())
}

async fn shutdown(state: AppState) {
    #[cfg(unix)]
    {
        let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down: saving open lines");
    state.lines.shutdown(&state).await;
}

async fn ready(axum::extract::State(state): axum::extract::State<AppState>) -> axum::http::StatusCode {
    match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        sqlx::query_scalar::<_, i32>("select 1").fetch_one(&state.db),
    ).await {
        Ok(Ok(1)) => axum::http::StatusCode::NO_CONTENT,
        _ => axum::http::StatusCode::SERVICE_UNAVAILABLE,
    }
}

pub fn router(state: AppState) -> Router {
    let max = state.cfg.max_upload_bytes;
    let v1 = Router::new()
        .merge(sheets::router())
        .merge(folders::router())
        .merge(formatting::router())
        .merge(sharing::router())
        .merge(visitors::router())
        .merge(transfers::router())
        .merge(search::router())
        .merge(lines::router())
        .merge(files::router())
        .merge(imports::router())
        .merge(code::router())
        .merge(javascript::router())
        .merge(comments::router())
        .merge(notify_preferences::router());
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(ready))
        .nest("/v1", v1)
        .layer(RequestBodyLimitLayer::new(max))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
