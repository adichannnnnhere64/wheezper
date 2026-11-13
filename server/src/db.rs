//! Database pool setup + migrations.

use anyhow::{Context, Result};
use sqlx::postgres::{PgPool, PgPoolOptions};
use std::time::Duration;

pub async fn connect(database_url: &str) -> Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect(database_url)
        .await
        .context("connecting to PostgreSQL")?;
    Ok(pool)
}

pub async fn migrate(pool: &PgPool) -> Result<()> {
    // `ignore_missing`: this checkout is missing the SQL for versions 3 and 4
    // ("assistant answer sequence", "session guidance") that the live database
    // already has. Those objects exist; refusing to start because the files are
    // gone would take the server down for a bookkeeping gap. New migrations must
    // therefore be numbered above the highest applied version (currently 5).
    sqlx::migrate!("./migrations")
        .set_ignore_missing(true)
        .run(pool)
        .await
        .context("running database migrations")
}
