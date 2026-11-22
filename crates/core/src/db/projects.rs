//! The projects the account can see, as Jira listed them last: the settings
//! show them without waiting for Jira, and without a connection.

use crate::models::Project;

/// Makes the cache hold `projects` and no other, in their order.
pub(crate) async fn replace(
    conn: &mut sqlx::SqliteConnection,
    projects: &[Project],
) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM projects")
        .execute(&mut *conn)
        .await?;
    for project in projects {
        sqlx::query("INSERT INTO projects (key, name) VALUES (?, ?)")
            .bind(&project.key)
            .bind(&project.name)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

/// The cached projects in the order Jira listed them.
pub(crate) async fn all(pool: &sqlx::SqlitePool) -> sqlx::Result<Vec<Project>> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT key, name FROM projects ORDER BY rowid")
            .fetch_all(pool)
            .await?;
    Ok(rows
        .into_iter()
        .map(|(key, name)| Project { key, name })
        .collect())
}
