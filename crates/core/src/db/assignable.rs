//! The assignee cache: who the picker offers, without asking Jira.

use crate::models::{IssueKey, User};
use sqlx::SqlitePool;

pub(crate) struct Cached {
    pub users: Vec<User>,
    /// Fetched more than an hour ago.
    pub stale: bool,
}

pub(crate) async fn get(pool: &SqlitePool, key: &IssueKey) -> sqlx::Result<Option<Cached>> {
    let row = sqlx::query!(
        r#"
        SELECT users, fetched_at < datetime('now', '-1 hour') AS "stale!: bool"
        FROM assignable_users
        WHERE issue_key = ?
        "#,
        key
    )
    .fetch_optional(pool)
    .await?;
    // An entry written in a format this version cannot read counts as a miss:
    // the fetch that follows overwrites it
    Ok(row.and_then(|row| {
        Some(Cached {
            users: serde_json::from_str(&row.users).ok()?,
            stale: row.stale,
        })
    }))
}

pub(crate) async fn put(pool: &SqlitePool, key: &IssueKey, users: &[User]) -> anyhow::Result<()> {
    let users = serde_json::to_string(users)?;
    sqlx::query!(
        "INSERT INTO assignable_users (issue_key, users) VALUES (?, ?)
         ON CONFLICT(issue_key) DO UPDATE SET
             users      = excluded.users,
             fetched_at = datetime('now')",
        key,
        users,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Forgets who the issue can be assigned to, so that the next open asks Jira again.
pub(crate) async fn forget(
    executor: impl sqlx::SqliteExecutor<'_>,
    key: &IssueKey,
) -> sqlx::Result<()> {
    sqlx::query!("DELETE FROM assignable_users WHERE issue_key = ?", key)
        .execute(executor)
        .await?;
    Ok(())
}
