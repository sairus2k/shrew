//! The active and future sprints, and which issue is in which.

use crate::models::IssueKey;
use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SprintRecord {
    pub id: i64,
    pub name: String,
    pub active: bool,
}

/// Makes the cache hold `sprints` and no other, and puts each issue in the
/// sprint `sprint_of` has for its id, or in none. Returns the keys of the
/// issues whose sprint is another one now, or has another name or state.
pub(crate) async fn replace(
    conn: &mut sqlx::SqliteConnection,
    sprints: &[SprintRecord],
    sprint_of: &BTreeMap<String, i64>,
) -> sqlx::Result<Vec<IssueKey>> {
    let stored: Vec<(i64, String, bool)> =
        sqlx::query_as("SELECT id, name, state = 'active' FROM sprints")
            .fetch_all(&mut *conn)
            .await?;
    let altered: HashSet<i64> = sprints
        .iter()
        .filter(|sprint| {
            !stored.iter().any(|(id, name, active)| {
                (*id, name, *active) == (sprint.id, &sprint.name, sprint.active)
            })
        })
        .map(|sprint| sprint.id)
        .collect();
    sqlx::query("DELETE FROM sprints")
        .execute(&mut *conn)
        .await?;
    for sprint in sprints {
        sqlx::query("INSERT INTO sprints (id, name, state) VALUES (?, ?, ?)")
            .bind(sprint.id)
            .bind(&sprint.name)
            .bind(if sprint.active { "active" } else { "future" })
            .execute(&mut *conn)
            .await?;
    }

    let mut changed = Vec::new();
    let in_sprints: Vec<(String, IssueKey, i64)> = sqlx::query_as(
        "SELECT id, key, sprint_id FROM issues WHERE sprint_id IS NOT NULL ORDER BY rowid",
    )
    .fetch_all(&mut *conn)
    .await?;
    for (id, key, old) in &in_sprints {
        let new = sprint_of.get(id);
        if new == Some(old) {
            if altered.contains(old) {
                changed.push(key.clone());
            }
            continue;
        }
        sqlx::query("UPDATE issues SET sprint_id = ? WHERE id = ?")
            .bind(new)
            .bind(id)
            .execute(&mut *conn)
            .await?;
        changed.push(key.clone());
    }
    let had_one: HashSet<&String> = in_sprints.iter().map(|(id, ..)| id).collect();
    for (id, sprint_id) in sprint_of {
        if had_one.contains(id) {
            continue;
        }
        // An issue the cache does not have is one the issue poll has yet to bring
        let key = sqlx::query_scalar("UPDATE issues SET sprint_id = ? WHERE id = ? RETURNING key")
            .bind(sprint_id)
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
        changed.extend(key);
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::SqlitePool;

    fn sprint(id: i64, name: &str, active: bool) -> SprintRecord {
        SprintRecord {
            id,
            name: name.to_owned(),
            active,
        }
    }

    /// WEB-1 to WEB-3 with the ids 1 to 3.
    async fn three_issues(pool: &SqlitePool) {
        for id in 1..=3 {
            sqlx::query(
                "INSERT INTO issues (id, key, summary, status, status_category, project_key,
                                     raw_json, remote_updated_at)
                 VALUES (?, 'WEB-' || ?, 'Summary', 'To Do', 'todo', 'WEB', '{}', '2026')",
            )
            .bind(id.to_string())
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
        }
    }

    async fn replaced(
        pool: &SqlitePool,
        sprints: &[SprintRecord],
        of: &[(&str, i64)],
    ) -> Vec<String> {
        let sprint_of = of
            .iter()
            .map(|(id, sprint)| (id.to_string(), *sprint))
            .collect();
        let mut conn = pool.acquire().await.unwrap();
        let changed = replace(&mut conn, sprints, &sprint_of).await.unwrap();
        let mut changed: Vec<String> = changed.iter().map(IssueKey::to_string).collect();
        changed.sort();
        changed
    }

    async fn stored(pool: &SqlitePool) -> Vec<(String, Option<i64>)> {
        sqlx::query_as("SELECT key, sprint_id FROM issues ORDER BY key")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    #[sqlx::test]
    async fn the_issues_are_put_in_their_sprints_and_taken_out_of_them(pool: SqlitePool) {
        three_issues(&pool).await;
        let both = [sprint(7, "Sprint 7", true), sprint(8, "Sprint 8", false)];

        // 404 is an issue the cache does not have
        let changed = replaced(&pool, &both, &[("1", 7), ("2", 7), ("404", 8)]).await;
        assert_eq!(changed, ["WEB-1", "WEB-2"]);

        // WEB-1 stays, WEB-2 moves to the next sprint, WEB-3 enters it
        let changed = replaced(&pool, &both, &[("1", 7), ("2", 8), ("3", 8)]).await;
        assert_eq!(changed, ["WEB-2", "WEB-3"]);
        let web = |n, sprint| (format!("WEB-{n}"), sprint);
        assert_eq!(
            stored(&pool).await,
            [web(1, Some(7)), web(2, Some(8)), web(3, Some(8))]
        );

        // Sprint 7 was completed
        let changed = replaced(&pool, &both[1..], &[("2", 8), ("3", 8)]).await;
        assert_eq!(changed, ["WEB-1"]);
        assert_eq!(
            stored(&pool).await,
            [web(1, None), web(2, Some(8)), web(3, Some(8))]
        );
        let sprints: Vec<i64> = sqlx::query_scalar("SELECT id FROM sprints")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(sprints, [8]);
    }

    #[sqlx::test]
    async fn the_issues_of_a_sprint_that_started_or_was_renamed_are_changed(pool: SqlitePool) {
        three_issues(&pool).await;
        let of = [("1", 7), ("2", 8)];
        let planned = [sprint(7, "Sprint 7", true), sprint(8, "Sprint 8", false)];
        replaced(&pool, &planned, &of).await;

        assert!(replaced(&pool, &planned, &of).await.is_empty());
        let started = [sprint(7, "Sprint 7", true), sprint(8, "Sprint 8", true)];
        assert_eq!(replaced(&pool, &started, &of).await, ["WEB-2"]);
        let renamed = [sprint(7, "Launch", true), sprint(8, "Sprint 8", true)];
        assert_eq!(replaced(&pool, &renamed, &of).await, ["WEB-1"]);
    }
}
