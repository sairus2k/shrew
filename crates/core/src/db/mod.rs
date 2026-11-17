use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode},
};
use std::path::Path;

pub(crate) mod issues;
pub(crate) mod sync_queue;
pub(crate) mod transitions;

const DB_FILE: &str = "shrew.db";

pub(crate) async fn init(data_dir: &Path) -> anyhow::Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(data_dir.join(DB_FILE))
        .create_if_missing(true)
        // Views keep reading while the sync engine writes
        .journal_mode(SqliteJournalMode::Wal);
    let pool = SqlitePool::connect_with(options).await?;
    sqlx::migrate!().run(&pool).await?;
    Ok(pool)
}

/// Deletes everything that came from Jira: the cache belongs to the site and
/// the account that filled it.
pub(crate) async fn clear(pool: &SqlitePool) -> sqlx::Result<()> {
    // The triggers on `issues` empty the search index
    sqlx::raw_sql(
        "BEGIN;
         DELETE FROM issues;
         DELETE FROM users;
         DELETE FROM projects;
         DELETE FROM sync_queue;
         DELETE FROM transition_cache;
         COMMIT;",
    )
    .execute(pool)
    .await?;
    Ok(())
}
