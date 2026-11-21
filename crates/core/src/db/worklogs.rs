//! The worklogs of the cached issues: the ones logged here and the ones Jira has.

use crate::models::{IssueKey, RejectedWorklog, SyncStatus, Worklog};
use chrono::{DateTime, SecondsFormat, Utc};

/// Jira refuses a worklog shorter than this ("You must indicate the time spent working").
pub(crate) const MIN_TIME_SPENT_SEC: i64 = 60;

/// A worklog logged here. `started_at` is UTC with a fixed width.
pub(crate) struct NewWorklog {
    pub issue_key: IssueKey,
    pub time_spent_sec: i64,
    pub started_at: String,
    pub comment: Option<String>,
}

/// A worklog as Jira has it, for an issue the cache holds.
pub(crate) struct RemoteWorklog {
    pub remote_id: String,
    pub issue_id: String,
    pub author_name: String,
    pub time_spent_sec: i64,
    pub started_at: String,
    pub comment: Option<String>,
    /// Set when the worklog is Jira's mirror of a Tempo worklog.
    pub tempo_id: Option<i64>,
}

/// Unix seconds as the stored `started_at`.
pub(crate) fn started_at_from_unix(seconds: i64) -> Option<String> {
    let instant = DateTime::from_timestamp(seconds, 0)?;
    Some(instant.to_rfc3339_opts(SecondsFormat::Millis, true))
}

/// Jira's `started`, such as `2026-03-29T01:30:00.000+0100`, as the stored value.
pub(crate) fn started_at_from_jira(started: &str) -> chrono::ParseResult<String> {
    let instant = DateTime::parse_from_str(started, "%Y-%m-%dT%H:%M:%S%.f%z")?;
    Ok(instant
        .with_timezone(&Utc)
        .to_rfc3339_opts(SecondsFormat::Millis, true))
}

/// The stored `started_at` in the form Jira's worklog call requires. Jira does
/// not accept the `Z` that the stored value ends in.
pub(crate) fn started_at_for_jira(started_at: &str) -> chrono::ParseResult<String> {
    let instant = DateTime::parse_from_rfc3339(started_at)?;
    Ok(instant.format("%Y-%m-%dT%H:%M:%S%.3f%z").to_string())
}

/// Returns the local id the row got.
pub(crate) async fn insert(
    executor: impl sqlx::SqliteExecutor<'_>,
    worklog: &NewWorklog,
) -> sqlx::Result<String> {
    sqlx::query_scalar(
        "INSERT INTO worklogs (issue_key, time_spent_sec, started_at, comment)
         VALUES (?, ?, ?, ?) RETURNING local_id",
    )
    .bind(&worklog.issue_key)
    .bind(worklog.time_spent_sec)
    .bind(&worklog.started_at)
    .bind(&worklog.comment)
    .fetch_one(executor)
    .await
}

/// Stores a worklog logged here and the outbox entry that delivers it. The
/// caller's transaction keeps the two together.
pub(crate) async fn log(
    conn: &mut sqlx::SqliteConnection,
    worklog: &NewWorklog,
) -> sqlx::Result<()> {
    let local_id = insert(&mut *conn, worklog).await?;
    // Which target it goes to is decided at delivery, so the entry holds only
    // what the user entered
    let payload = serde_json::json!({
        "local_id": local_id,
        "time_spent_seconds": worklog.time_spent_sec,
        "started_at": worklog.started_at,
        "comment": worklog.comment,
    });
    super::sync_queue::enqueue(
        &mut *conn,
        "worklog",
        &worklog.issue_key,
        &payload.to_string(),
    )
    .await
}

/// Records Jira's id for a worklog that has been delivered.
pub(crate) async fn confirm(
    executor: impl sqlx::SqliteExecutor<'_>,
    local_id: &str,
    remote_id: &str,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE worklogs SET remote_id = ?, sync_status = 'synced' WHERE local_id = ?")
        .bind(remote_id)
        .bind(local_id)
        .execute(executor)
        .await?;
    Ok(())
}

/// Records Tempo's id for a worklog that has been delivered to Tempo. Jira's
/// id follows when the poll returns the mirror.
pub(crate) async fn confirm_tempo(
    executor: impl sqlx::SqliteExecutor<'_>,
    local_id: &str,
    tempo_id: i64,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE worklogs SET tempo_id = ?, sync_status = 'synced' WHERE local_id = ?")
        .bind(tempo_id)
        .bind(local_id)
        .execute(executor)
        .await?;
    Ok(())
}

pub(crate) async fn mark_failed(
    executor: impl sqlx::SqliteExecutor<'_>,
    local_id: &str,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE worklogs SET sync_status = 'failed' WHERE local_id = ?")
        .bind(local_id)
        .execute(executor)
        .await?;
    Ok(())
}

/// The worklog Jira has under `remote_id` replaces the cached one, so a worklog
/// logged here and coming back through the poll stays a single row. A worklog
/// of an issue the cache does not have is not stored. Returns the key of the
/// issue, or `None` if it was not stored.
pub(crate) async fn upsert_remote(
    conn: &mut sqlx::SqliteConnection,
    worklog: &RemoteWorklog,
) -> sqlx::Result<Option<IssueKey>> {
    if let Some(tempo_id) = worklog.tempo_id {
        // The row posted to Tempo from here gets Jira's id, so the upsert below finds it
        sqlx::query("UPDATE worklogs SET remote_id = ? WHERE tempo_id = ? AND remote_id IS NULL")
            .bind(&worklog.remote_id)
            .bind(tempo_id)
            .execute(&mut *conn)
            .await?;
    }
    // Tempo does not copy the description into the mirror (Jira shows a fixed
    // text there), so a worklog posted to Tempo from here keeps its comment
    sqlx::query_scalar(
        "INSERT INTO worklogs
             (remote_id, issue_key, author_name, time_spent_sec, started_at, comment, sync_status)
         SELECT ?, key, ?, ?, ?, ?, 'synced' FROM issues WHERE id = ?
         ON CONFLICT(remote_id) DO UPDATE SET
             issue_key      = excluded.issue_key,
             author_name    = excluded.author_name,
             time_spent_sec = excluded.time_spent_sec,
             started_at     = excluded.started_at,
             comment        = CASE WHEN worklogs.tempo_id IS NULL
                                   THEN excluded.comment ELSE worklogs.comment END,
             sync_status    = 'synced'
         RETURNING issue_key",
    )
    .bind(&worklog.remote_id)
    .bind(&worklog.author_name)
    .bind(worklog.time_spent_sec)
    .bind(&worklog.started_at)
    .bind(&worklog.comment)
    .bind(&worklog.issue_id)
    .fetch_optional(conn)
    .await
}

/// Removes the cached worklogs Jira deleted. A worklog logged here and not yet
/// delivered has no `remote_id`, so it is never matched. Returns the keys of the
/// issues that lost one.
pub(crate) async fn delete_remote(
    conn: &mut sqlx::SqliteConnection,
    remote_ids: &[i64],
) -> sqlx::Result<Vec<IssueKey>> {
    sqlx::query_scalar(
        "DELETE FROM worklogs
         WHERE remote_id IN (SELECT value FROM json_each(?))
         RETURNING issue_key",
    )
    .bind(serde_json::json!(remote_ids.iter().map(i64::to_string).collect::<Vec<_>>()).to_string())
    .fetch_all(conn)
    .await
}

/// The worklogs of the issue, the one started last first. A worklog logged
/// here has no author name until Jira sends it back.
pub(crate) async fn for_issue(
    executor: impl sqlx::SqliteExecutor<'_>,
    key: &IssueKey,
) -> sqlx::Result<Vec<Worklog>> {
    type Row = (
        Option<String>,
        String,
        i64,
        Option<String>,
        SyncStatus,
        Option<i64>,
        Option<String>,
    );
    // The outbox entry of a refused worklog holds the reason
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT w.author_name, w.started_at, w.time_spent_sec, w.comment, w.sync_status,
                q.id, q.error_msg
         FROM worklogs w
         LEFT JOIN sync_queue q
                ON q.entity_type = 'worklog' AND q.status = 'failed'
               AND json_extract(q.payload, '$.local_id') = w.local_id
         WHERE w.issue_key = ? ORDER BY w.started_at DESC, w.created_at DESC",
    )
    .bind(key)
    .fetch_all(executor)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(author, started_at, seconds, comment, status, entry_id, error)| Worklog {
                author_name: author.unwrap_or_else(|| "You".to_owned()),
                started_at: DateTime::parse_from_rfc3339(&started_at)
                    .map_or(0, |at| at.timestamp()),
                time_spent_secs: seconds.try_into().unwrap_or_default(),
                comment,
                status,
                rejected: entry_id.map(|entry_id| RejectedWorklog {
                    entry_id,
                    reason: error.unwrap_or_default(),
                }),
            },
        )
        .collect())
}

/// Puts a refused worklog back in the queue. Returns the key of its issue, or
/// `None` if the entry is not a refused worklog.
pub(crate) async fn retry(
    pool: &sqlx::SqlitePool,
    entry_id: i64,
) -> sqlx::Result<Option<IssueKey>> {
    let mut tx = pool.begin().await?;
    let entry: Option<(IssueKey, String)> = sqlx::query_as(
        "UPDATE sync_queue SET status = 'pending', retry_count = 0, error_msg = NULL
         WHERE id = ? AND entity_type = 'worklog' AND status = 'failed'
         RETURNING issue_key, json_extract(payload, '$.local_id')",
    )
    .bind(entry_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((key, local_id)) = entry else {
        return Ok(None);
    };
    sqlx::query("UPDATE worklogs SET sync_status = 'pending' WHERE local_id = ?")
        .bind(local_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Some(key))
}

/// Deletes a refused worklog and its outbox entry. Returns the key of its
/// issue, or `None` if the entry is not a refused worklog.
pub(crate) async fn discard(
    pool: &sqlx::SqlitePool,
    entry_id: i64,
) -> sqlx::Result<Option<IssueKey>> {
    let mut tx = pool.begin().await?;
    let entry: Option<(IssueKey, String)> = sqlx::query_as(
        "DELETE FROM sync_queue
         WHERE id = ? AND entity_type = 'worklog' AND status = 'failed'
         RETURNING issue_key, json_extract(payload, '$.local_id')",
    )
    .bind(entry_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((key, local_id)) = entry else {
        return Ok(None);
    };
    sqlx::query("DELETE FROM worklogs WHERE local_id = ?")
        .bind(local_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Some(key))
}

/// Follows an issue that was moved to another project: its worklogs, and the
/// worklogs still waiting in the outbox, belong to the new key.
pub(crate) async fn rename_issue(
    conn: &mut sqlx::SqliteConnection,
    old: &IssueKey,
    new: &IssueKey,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE worklogs SET issue_key = ? WHERE issue_key = ?")
        .bind(new)
        .bind(old)
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "UPDATE sync_queue SET issue_key = ? WHERE issue_key = ? AND entity_type = 'worklog'",
    )
    .bind(new)
    .bind(old)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub(crate) async fn delete_for_issue(
    executor: impl sqlx::SqliteExecutor<'_>,
    key: &IssueKey,
) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM worklogs WHERE issue_key = ?")
        .bind(key)
        .execute(executor)
        .await?;
    Ok(())
}
