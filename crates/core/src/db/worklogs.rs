//! The worklogs of the cached issues: the ones logged here and the ones Jira has.

use crate::models::{IssueKey, SyncStatus, Worklog};
use chrono::{DateTime, SecondsFormat, Utc};

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
    executor: impl sqlx::SqliteExecutor<'_>,
    worklog: &RemoteWorklog,
) -> sqlx::Result<Option<IssueKey>> {
    sqlx::query_scalar(
        "INSERT INTO worklogs
             (remote_id, issue_key, author_name, time_spent_sec, started_at, comment, sync_status)
         SELECT ?, key, ?, ?, ?, ?, 'synced' FROM issues WHERE id = ?
         ON CONFLICT(remote_id) DO UPDATE SET
             issue_key      = excluded.issue_key,
             author_name    = excluded.author_name,
             time_spent_sec = excluded.time_spent_sec,
             started_at     = excluded.started_at,
             comment        = excluded.comment,
             sync_status    = 'synced'
         RETURNING issue_key",
    )
    .bind(&worklog.remote_id)
    .bind(&worklog.author_name)
    .bind(worklog.time_spent_sec)
    .bind(&worklog.started_at)
    .bind(&worklog.comment)
    .bind(&worklog.issue_id)
    .fetch_optional(executor)
    .await
}

/// The worklogs of the issue, the one started last first. A worklog logged
/// here has no author name until Jira sends it back.
pub(crate) async fn for_issue(
    executor: impl sqlx::SqliteExecutor<'_>,
    key: &IssueKey,
) -> sqlx::Result<Vec<Worklog>> {
    type Row = (Option<String>, String, i64, Option<String>, SyncStatus);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT author_name, started_at, time_spent_sec, comment, sync_status
         FROM worklogs WHERE issue_key = ? ORDER BY started_at DESC, created_at DESC",
    )
    .bind(key)
    .fetch_all(executor)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(author, started_at, seconds, comment, status)| Worklog {
            author_name: author.unwrap_or_else(|| "You".to_owned()),
            started_at: DateTime::parse_from_rfc3339(&started_at).map_or(0, |at| at.timestamp()),
            time_spent_secs: seconds.try_into().unwrap_or_default(),
            comment,
            status,
        })
        .collect())
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
