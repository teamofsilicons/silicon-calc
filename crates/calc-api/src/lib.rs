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
pub mod search;
pub mod sharing;
pub mod sheets;
pub mod transfers;

// Lines: live editing, history, copies, files, code, comments, notifications.
pub mod code;
pub mod comments;
pub mod files;
pub mod lines;
pub mod notify;

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
    lines::spawn_maintenance(state.clone());
}

/// Runs the service until Ctrl-C, then saves every open line.
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
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down: saving open lines");
    state.lines.shutdown(&state).await;
}

pub fn router(state: AppState) -> Router {
    let max = state.cfg.max_upload_bytes;
    let v1 = Router::new()
        .merge(sheets::router())
        .merge(folders::router())
        .merge(sharing::router())
        .merge(transfers::router())
        .merge(search::router())
        .merge(lines::router())
        .merge(files::router())
        .merge(code::router())
        .merge(comments::router());
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .nest("/v1", v1)
        .layer(RequestBodyLimitLayer::new(max))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
