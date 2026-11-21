//! The outbox: local changes that Jira has not confirmed yet.

use crate::models::IssueKey;
use sqlx::SqlitePool;

pub(crate) struct QueueEntry {
    pub id: i64,
    pub entity_type: String,
    pub issue_key: IssueKey,
    pub payload: String,
    pub retry_count: i64,
}

/// Adds a change to send to Jira. `payload` is the body of the request.
pub(crate) async fn enqueue(
    executor: impl sqlx::SqliteExecutor<'_>,
    entity_type: &str,
    issue_key: &IssueKey,
    payload: &str,
) -> sqlx::Result<()> {
    sqlx::query!(
        "INSERT INTO sync_queue (entity_type, issue_key, operation, payload)
         VALUES (?, ?, 'create', ?)",
        entity_type,
        issue_key,
        payload,
    )
    .execute(executor)
    .await?;
    Ok(())
}

/// Marks the oldest entry that still has to be sent as `in_progress` and
/// returns it. Worklogs are passed over when `skip_worklogs` is set.
// `created_at` has a resolution of one second; the id is the creation order.
// Only one processor runs, so an entry that is `in_progress` here was left by
// a crash or a quit in the middle of its delivery.
pub(crate) async fn claim_next(
    pool: &SqlitePool,
    skip_worklogs: bool,
) -> sqlx::Result<Option<QueueEntry>> {
    sqlx::query_as!(
        QueueEntry,
        r#"
        UPDATE sync_queue SET status = 'in_progress'
        WHERE id = (
            SELECT id FROM sync_queue
            WHERE status IN ('pending', 'in_progress')
              AND NOT (? AND entity_type = 'worklog')
            ORDER BY id
            LIMIT 1
        )
        RETURNING id AS "id!", entity_type, issue_key AS "issue_key: IssueKey",
                  payload, retry_count
        "#,
        skip_worklogs
    )
    .fetch_optional(pool)
    .await
}

pub(crate) async fn mark_synced(
    executor: impl sqlx::SqliteExecutor<'_>,
    id: i64,
) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE sync_queue SET status = 'synced', error_msg = NULL, synced_at = datetime('now')
         WHERE id = ?",
        id
    )
    .execute(executor)
    .await?;
    Ok(())
}

/// Puts a claimed entry back without counting the attempt.
pub(crate) async fn release(pool: &SqlitePool, id: i64) -> sqlx::Result<()> {
    sqlx::query!("UPDATE sync_queue SET status = 'pending' WHERE id = ?", id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Puts a claimed entry back for another attempt and counts this one.
pub(crate) async fn release_for_retry(pool: &SqlitePool, id: i64, error: &str) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE sync_queue SET status = 'pending', retry_count = retry_count + 1, error_msg = ?
         WHERE id = ?",
        error,
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub(crate) async fn mark_failed(pool: &SqlitePool, id: i64, error: &str) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE sync_queue SET status = 'failed', error_msg = ? WHERE id = ?",
        error,
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Removes a status change that will not reach Jira. The user is told by the
/// caller, and a rollback that could not run is found again by the issue row
/// (`issues::unconfirmed`), so nothing needs the entry afterwards.
pub(crate) async fn remove(pool: &SqlitePool, id: i64) -> sqlx::Result<()> {
    sqlx::query!("DELETE FROM sync_queue WHERE id = ?", id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Follows an issue that was moved to another project: every entry of it,
/// also one Jira refused and the issue still shows, belongs to the new key.
pub(crate) async fn rename_issue(
    executor: impl sqlx::SqliteExecutor<'_>,
    old: &IssueKey,
    new: &IssueKey,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE sync_queue SET issue_key = ? WHERE issue_key = ?")
        .bind(new)
        .bind(old)
        .execute(executor)
        .await?;
    Ok(())
}

/// Removes every entry of an issue that no longer exists, whatever its state:
/// there is no view left to show a refused comment in, or to retry it from.
/// Returns the entity type of each.
pub(crate) async fn remove_for_issue(
    executor: impl sqlx::SqliteExecutor<'_>,
    key: &IssueKey,
) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar!(
        "DELETE FROM sync_queue WHERE issue_key = ? RETURNING entity_type",
        key
    )
    .fetch_all(executor)
    .await
}

/// Deletes the refused worklogs of an issue, which no view can show once the
/// worklog rows of the issue are gone.
pub(crate) async fn remove_failed_worklogs(
    executor: impl sqlx::SqliteExecutor<'_>,
    key: &IssueKey,
) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM sync_queue WHERE issue_key = ? AND entity_type = 'worklog' AND status = 'failed'")
        .bind(key)
        .execute(executor)
        .await?;
    Ok(())
}

/// Drops the status changes of an issue that have not been sent. Returns how
/// many there were.
pub(crate) async fn drop_unsent_transitions(
    executor: impl sqlx::SqliteExecutor<'_>,
    key: &IssueKey,
) -> sqlx::Result<u64> {
    let dropped = sqlx::query!(
        "DELETE FROM sync_queue
         WHERE issue_key = ? AND entity_type = 'transition'
           AND status IN ('pending', 'in_progress')",
        key
    )
    .execute(executor)
    .await?;
    Ok(dropped.rows_affected())
}

/// Puts a comment Jira refused back in the queue. Returns the key of its
/// issue, or `None` if the entry is not a refused comment.
pub(crate) async fn retry_comment(pool: &SqlitePool, id: i64) -> sqlx::Result<Option<IssueKey>> {
    sqlx::query_scalar!(
        r#"UPDATE sync_queue SET status = 'pending', retry_count = 0, error_msg = NULL
           WHERE id = ? AND entity_type = 'comment' AND status = 'failed'
           RETURNING issue_key AS "issue_key: IssueKey""#,
        id
    )
    .fetch_optional(pool)
    .await
}

/// Deletes a comment Jira refused. Returns the key of its issue, or `None` if
/// the entry is not a refused comment.
pub(crate) async fn discard_comment(pool: &SqlitePool, id: i64) -> sqlx::Result<Option<IssueKey>> {
    sqlx::query_scalar!(
        r#"DELETE FROM sync_queue
           WHERE id = ? AND entity_type = 'comment' AND status = 'failed'
           RETURNING issue_key AS "issue_key: IssueKey""#,
        id
    )
    .fetch_optional(pool)
    .await
}

/// How many changes Jira has not received yet. A failed one is not counted:
/// Jira refused it for good.
pub(crate) async fn unsent(pool: &SqlitePool) -> sqlx::Result<i64> {
    sqlx::query_scalar!(
        "SELECT COUNT(*) FROM sync_queue WHERE status IN ('pending', 'in_progress')"
    )
    .fetch_one(pool)
    .await
}
