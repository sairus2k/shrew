use crate::models::Account;
use sqlx::{
    Sqlite, SqlitePool, Transaction,
    sqlite::{SqliteConnectOptions, SqliteJournalMode},
};
use std::path::Path;

pub(crate) mod assignable;
pub(crate) mod issues;
pub(crate) mod sprints;
pub(crate) mod sync_queue;
pub(crate) mod sync_state;
pub(crate) mod timer;
pub(crate) mod transitions;
pub(crate) mod worklogs;

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

/// Begins a transaction that writes.
// The write lock is taken here and not at the first write. SQLite does not
// wait when a transaction that has already read asks for it while another
// connection is writing: it refuses at once with "database is locked", and
// the sync engine writes in every cycle.
pub(crate) async fn begin(pool: &SqlitePool) -> sqlx::Result<Transaction<'static, Sqlite>> {
    pool.begin_with("BEGIN IMMEDIATE").await
}

/// Deletes everything that came from Jira, and how far the sync has read it:
/// the cache belongs to the site and the account that filled it.
pub(crate) async fn clear(pool: &SqlitePool) -> sqlx::Result<()> {
    // The triggers on `issues` empty the search index
    sqlx::raw_sql(
        "BEGIN IMMEDIATE;
         DELETE FROM issues;
         DELETE FROM sprints;
         DELETE FROM users;
         DELETE FROM projects;
         DELETE FROM sync_queue;
         DELETE FROM transition_cache;
         DELETE FROM assignable_users;
         DELETE FROM worklogs;
         DELETE FROM running_timer;
         DELETE FROM cache_owner;
         DELETE FROM watched_projects;
         INSERT OR REPLACE INTO sync_state (id) VALUES (1);
         COMMIT;",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// The account whose data the database holds. `None` for an empty database,
/// and for one that was last used by a build that did not record it.
pub(crate) async fn owner(pool: &SqlitePool) -> sqlx::Result<Option<Account>> {
    let row: Option<(String, String, String)> =
        sqlx::query_as("SELECT site_url, account_id, email FROM cache_owner")
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(site_url, account_id, email)| Account {
        site_url,
        email,
        account_id,
    }))
}

pub(crate) async fn set_owner(pool: &SqlitePool, owner: &Account) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT OR REPLACE INTO cache_owner (id, site_url, account_id, email) VALUES (1, ?, ?, ?)",
    )
    .bind(&owner.site_url)
    .bind(&owner.account_id)
    .bind(&owner.email)
    .execute(pool)
    .await?;
    Ok(())
}
