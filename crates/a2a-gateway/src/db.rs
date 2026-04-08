use anyhow::{Context, Result};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool};

pub type Db = PgPool;

/// Open (or create) the PostgreSQL connection pool and run all migrations.
///
/// `url` should be a full postgres:// connection string, e.g.:
///   postgres://a2a:secret@localhost:5432/a2a
///
/// Pool is configured with:
///   - max 20 connections (safe for a single-gateway deployment)
///   - 30s acquire timeout (prevents indefinite waits under load)
///
/// In tests, point at a real Postgres instance via DATABASE_URL or use
/// a per-test schema to isolate state.
pub async fn open(url: &str) -> Result<Db> {
    let pool = PgPoolOptions::new()
        .max_connections(20)
        .acquire_timeout(std::time::Duration::from_secs(30))
        .connect(url)
        .await
        .with_context(|| format!("connecting to postgres: {url}"))?;

    run_migrations(&pool).await?;
    Ok(pool)
}

async fn run_migrations(pool: &Db) -> Result<()> {
    // Use raw_sql (simple query protocol) so multi-statement .sql files work with Postgres.
    pool.execute(sqlx::raw_sql(include_str!("migrations/001_init.sql")))
        .await
        .context("running migration 001_init")?;

    pool.execute(sqlx::raw_sql(include_str!("migrations/002_projects.sql")))
        .await
        .context("running migration 002_projects")?;

    pool.execute(sqlx::raw_sql(include_str!("migrations/003_file_sync.sql")))
        .await
        .context("running migration 003_file_sync")?;

    pool.execute(sqlx::raw_sql(include_str!("migrations/004_task_log.sql")))
        .await
        .context("running migration 004_task_log")?;

    pool.execute(sqlx::raw_sql(include_str!("migrations/005_contributions.sql")))
        .await
        .context("running migration 005_contributions")?;

    pool.execute(sqlx::raw_sql(include_str!("migrations/006_groups.sql")))
        .await
        .context("running migration 006_groups")?;

    pool.execute(sqlx::raw_sql(include_str!("migrations/007_task_status.sql")))
        .await
        .context("running migration 007_task_status")?;

    Ok(())
}
