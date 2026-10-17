//! The Postgres pool and migrations.

use std::time::Duration;

use sqlx::{PgPool, postgres::PgPoolOptions};

pub async fn connect(url: &str) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(32)
        .acquire_timeout(Duration::from_secs(10))
        .connect(url)
        .await?;
    sqlx::migrate!("../../migrations").run(&pool).await?;
    Ok(pool)
}
