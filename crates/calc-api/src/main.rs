//! Starts the Silicon Calc service.

use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_env("CALC_LOG").unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn")))
        .init();
    calc_api::serve(calc_api::config::Config::from_env()?).await
}
