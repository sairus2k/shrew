//! The transition cache: what the status menu offers, without asking Jira.

use crate::models::{IssueKey, Transition};
use sqlx::SqlitePool;

/// What decides the transitions an issue offers: its workflow, which Jira
/// picks by project and issue type, and the status it is in.
#[derive(Clone)]
pub(crate) struct Position {
    pub project_key: String,
    pub issue_type: String,
    pub status: String,
}

pub(crate) struct Cached {
    pub transitions: Vec<Transition>,
    /// Fetched more than an hour ago.
    pub stale: bool,
}

/// `None` for an issue that is not in the cache or was stored without its type.
pub(crate) async fn position(pool: &SqlitePool, key: &IssueKey) -> sqlx::Result<Option<Position>> {
    sqlx::query_as!(
        Position,
        r#"
        SELECT project_key, issue_type AS "issue_type!", status
        FROM issues
        WHERE key = ? AND issue_type IS NOT NULL
        "#,
        key
    )
    .fetch_optional(pool)
    .await
}

pub(crate) async fn get(pool: &SqlitePool, position: &Position) -> sqlx::Result<Option<Cached>> {
    let row = sqlx::query!(
        r#"
        SELECT transitions, fetched_at < datetime('now', '-1 hour') AS "stale!: bool"
        FROM transition_cache
        WHERE project_key = ? AND issue_type = ? AND from_status = ?
        "#,
        position.project_key,
        position.issue_type,
        position.status,
    )
    .fetch_optional(pool)
    .await?;
    // An entry written in a format this version cannot read counts as a miss:
    // the fetch that follows overwrites it
    Ok(row.and_then(|row| {
        Some(Cached {
            transitions: serde_json::from_str(&row.transitions).ok()?,
            stale: row.stale,
        })
    }))
}

pub(crate) async fn put(
    pool: &SqlitePool,
    position: &Position,
    transitions: &[Transition],
) -> anyhow::Result<()> {
    let transitions = serde_json::to_string(transitions)?;
    sqlx::query!(
        "INSERT INTO transition_cache (project_key, issue_type, from_status, transitions)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(project_key, issue_type, from_status) DO UPDATE SET
             transitions = excluded.transitions,
             fetched_at  = datetime('now')",
        position.project_key,
        position.issue_type,
        position.status,
        transitions,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Up to `limit` positions that a cached issue is in and the cache has no
/// entry for, each with an issue to ask Jira through. An issue whose change
/// waits in the outbox is not used: Jira has it in another status.
pub(crate) async fn missing(
    pool: &SqlitePool,
    limit: i64,
) -> sqlx::Result<Vec<(IssueKey, Position)>> {
    let rows = sqlx::query!(
        r#"
        SELECT MIN(key) AS "key!: IssueKey", project_key AS "project_key!",
               issue_type AS "issue_type!", status AS "status!"
        FROM issues
        WHERE issue_type IS NOT NULL AND sync_status = 'synced' AND NOT EXISTS (
            SELECT 1 FROM transition_cache
            WHERE project_key = issues.project_key
              AND issue_type = issues.issue_type
              AND from_status = issues.status
        )
        GROUP BY project_key, issue_type, status
        -- A position Jira keeps failing to answer for must not hold the
        -- places of the limit in every pass
        ORDER BY RANDOM()
        LIMIT ?
        "#,
        limit
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let position = Position {
                project_key: row.project_key,
                issue_type: row.issue_type,
                status: row.status,
            };
            (row.key, position)
        })
        .collect())
}

/// Forgets what the status offers, so that the next open asks Jira again.
pub(crate) async fn forget(
    executor: impl sqlx::SqliteExecutor<'_>,
    position: &Position,
) -> sqlx::Result<()> {
    sqlx::query!(
        "DELETE FROM transition_cache
         WHERE project_key = ? AND issue_type = ? AND from_status = ?",
        position.project_key,
        position.issue_type,
        position.status,
    )
    .execute(executor)
    .await?;
    Ok(())
}
