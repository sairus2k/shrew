//! How far the polls have read Jira, and the projects they read. It is kept
//! beside the rows it describes, so that a page and the position it moves are
//! one transaction.

use sqlx::{SqliteConnection, SqliteExecutor, SqlitePool};

// The field names are those of the `config.json` that held this state before
// the database did: `auth::legacy_sync_state` reads it from there
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct SyncState {
    pub watched_projects: Vec<String>, // project keys, in the order they were chosen
    // Watched projects added after the initial sync whose issues are not all
    // downloaded yet
    pub pending_projects: Vec<String>,
    // When the last sync started, in Unix milliseconds. `None` until the
    // initial sync has finished
    pub last_sync_at: Option<i64>,
    pub last_reconcile_at: i64,
    pub last_worklog_sync_at: i64,
    pub last_worklog_delete_sync_at: i64,
}

/// A position that a poll moves forward.
#[derive(Clone, Copy)]
pub(crate) enum Position {
    Issues,
    Worklogs,
    DeletedWorklogs,
}

pub(crate) async fn load(pool: &SqlitePool) -> sqlx::Result<SyncState> {
    let (last_sync_at, last_reconcile_at, last_worklog_sync_at, last_worklog_delete_sync_at) =
        sqlx::query_as(
            "SELECT last_sync_at, last_reconcile_at, last_worklog_sync_at,
                    last_worklog_delete_sync_at
             FROM sync_state",
        )
        .fetch_one(pool)
        .await?;
    let projects: Vec<(String, bool)> =
        sqlx::query_as("SELECT key, pending FROM watched_projects ORDER BY position")
            .fetch_all(pool)
            .await?;
    Ok(SyncState {
        pending_projects: projects
            .iter()
            .filter(|(_, pending)| *pending)
            .map(|(key, _)| key.clone())
            .collect(),
        watched_projects: projects.into_iter().map(|(key, _)| key).collect(),
        last_sync_at,
        last_reconcile_at,
        last_worklog_sync_at,
        last_worklog_delete_sync_at,
    })
}

pub(crate) async fn watched(executor: impl SqliteExecutor<'_>) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar("SELECT key FROM watched_projects ORDER BY position")
        .fetch_all(executor)
        .await
}

/// Replaces the watched projects. After the initial sync a project that was
/// not watched before is pending, and one that was pending stays so.
pub(crate) async fn watch(
    conn: &mut SqliteConnection,
    project_keys: &[String],
) -> sqlx::Result<()> {
    let synced: bool = sqlx::query_scalar("SELECT last_sync_at IS NOT NULL FROM sync_state")
        .fetch_one(&mut *conn)
        .await?;
    let before: Vec<(String, bool)> = sqlx::query_as("SELECT key, pending FROM watched_projects")
        .fetch_all(&mut *conn)
        .await?;
    sqlx::query("DELETE FROM watched_projects")
        .execute(&mut *conn)
        .await?;
    for (position, key) in project_keys.iter().enumerate() {
        let pending = synced
            && before
                .iter()
                .find(|(watched, _)| watched == key)
                .is_none_or(|(_, pending)| *pending);
        sqlx::query(
            "INSERT OR IGNORE INTO watched_projects (key, position, pending) VALUES (?, ?, ?)",
        )
        .bind(key)
        .bind(position as i64)
        .bind(pending)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Moves `position` from `from` to `to`, in the transaction of the page that
/// was read up to `to`.
// Nothing moves when the position is no longer at `from`: a sign-in during
// the poll started a new setup, and its position is not the poll's to set
pub(crate) async fn advance(
    executor: impl SqliteExecutor<'_>,
    position: Position,
    from: i64,
    to: i64,
) -> sqlx::Result<()> {
    let column = match position {
        Position::Issues => "last_sync_at",
        Position::Worklogs => "last_worklog_sync_at",
        Position::DeletedWorklogs => "last_worklog_delete_sync_at",
    };
    let sql = format!("UPDATE sync_state SET {column} = ? WHERE {column} = ?");
    sqlx::query(&sql)
        .bind(to)
        .bind(from)
        .execute(executor)
        .await?;
    Ok(())
}

/// Every issue of the project is stored: it is no longer pending.
pub(crate) async fn downloaded(
    executor: impl SqliteExecutor<'_>,
    project_key: &str,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE watched_projects SET pending = 0 WHERE key = ?")
        .bind(project_key)
        .execute(executor)
        .await?;
    Ok(())
}

/// The first incremental poll starts from `started_at`. A sync of projects
/// added later leaves the time where the polls have moved it.
pub(crate) async fn finish_initial_sync(
    executor: impl SqliteExecutor<'_>,
    started_at: i64,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE sync_state SET last_sync_at = COALESCE(last_sync_at, ?)")
        .bind(started_at)
        .execute(executor)
        .await?;
    Ok(())
}

pub(crate) async fn reconciled(
    executor: impl SqliteExecutor<'_>,
    started_at: i64,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE sync_state SET last_reconcile_at = ?")
        .bind(started_at)
        .execute(executor)
        .await?;
    Ok(())
}

/// Takes over the state a build before this table kept in `config.json`.
/// Does nothing when the database already watches projects: a start that
/// stopped before the config was rewritten has imported it.
pub(crate) async fn import(pool: &SqlitePool, legacy: &SyncState) -> sqlx::Result<()> {
    let mut tx = crate::db::begin(pool).await?;
    if !watched(&mut *tx).await?.is_empty() {
        return Ok(());
    }
    // The positions describe the issues the database had. One that was
    // deleted or replaced since is empty, and gets the initial sync instead
    // of polls that would never fill it.
    let filled: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM issues)")
        .fetch_one(&mut *tx)
        .await?;
    for (position, key) in legacy.watched_projects.iter().enumerate() {
        sqlx::query(
            "INSERT OR IGNORE INTO watched_projects (key, position, pending) VALUES (?, ?, ?)",
        )
        .bind(key)
        .bind(position as i64)
        .bind(filled && legacy.pending_projects.contains(key))
        .execute(&mut *tx)
        .await?;
    }
    if filled {
        sqlx::query(
            "UPDATE sync_state SET last_sync_at = ?, last_reconcile_at = ?,
                    last_worklog_sync_at = ?, last_worklog_delete_sync_at = ?",
        )
        .bind(legacy.last_sync_at)
        .bind(legacy.last_reconcile_at)
        .bind(legacy.last_worklog_sync_at)
        .bind(legacy.last_worklog_delete_sync_at)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// Puts the state where an initial sync of `projects` that started at
/// `synced_at` leaves it.
#[cfg(any(test, feature = "test-support"))]
pub(crate) async fn set_synced(
    pool: &SqlitePool,
    projects: &[&str],
    synced_at: i64,
) -> sqlx::Result<()> {
    let mut tx = crate::db::begin(pool).await?;
    sqlx::query("DELETE FROM watched_projects")
        .execute(&mut *tx)
        .await?;
    for (position, key) in projects.iter().enumerate() {
        sqlx::query("INSERT INTO watched_projects (key, position) VALUES (?, ?)")
            .bind(key)
            .bind(position as i64)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("UPDATE sync_state SET last_sync_at = ?")
        .bind(synced_at)
        .execute(&mut *tx)
        .await?;
    tx.commit().await
}
