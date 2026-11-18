use crate::{
    adf,
    models::{
        Comment, IssueDetail, IssueKey, IssueRow, RejectedComment, StatusCategory, SyncStatus,
    },
};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;
use sqlx::SqlitePool;

/// The moment Jira last changed an issue.
// Jira sends `updated` with the UTC offset of the requesting user's time zone,
// which changes with daylight saving time and with the profile setting. The
// stale-write guard in `upsert` compares the column as text, so the value is
// held in UTC with a fixed width: only then does text order equal time order.
#[derive(Clone, Debug, PartialEq, Eq, sqlx::Type)]
#[sqlx(transparent)]
pub(crate) struct JiraTimestamp(String);

impl JiraTimestamp {
    /// Parses Jira's `updated` value, such as `2026-03-29T01:30:00.000+0100`.
    pub(crate) fn parse(updated: &str) -> chrono::ParseResult<Self> {
        let instant = DateTime::parse_from_str(updated, "%Y-%m-%dT%H:%M:%S%.f%z")?;
        Ok(Self(
            instant
                .with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Millis, true),
        ))
    }
}

/// An issue as it is written to the cache.
pub(crate) struct IssueRecord {
    pub id: String,
    pub key: IssueKey,
    pub summary: String,
    pub description_text: Option<String>,
    pub status: String,
    pub status_category: StatusCategory,
    pub assignee_id: Option<String>,
    pub assignee_name: Option<String>,
    pub project_key: String,
    pub issue_type: Option<String>,
    pub raw_json: String,
    pub remote_updated_at: JiraTimestamp,
}

/// Returns whether the row was written: `false` means the cache already holds
/// this version of the issue or a newer one.
pub(crate) async fn upsert(
    executor: impl sqlx::SqliteExecutor<'_>,
    issue: &IssueRecord,
) -> sqlx::Result<bool> {
    let written = sqlx::query!(
        r#"
        INSERT INTO issues
            (id, key, summary, description_text, status, status_category,
             assignee_id, assignee_name, project_key, issue_type, raw_json,
             remote_updated_at, synced_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, datetime('now'))
        -- Conflict on id, not key: a moved issue keeps its id but gets a new key
        ON CONFLICT(id) DO UPDATE SET
            key                = excluded.key,
            project_key        = excluded.project_key,
            summary            = excluded.summary,
            description_text   = excluded.description_text,
            status             = excluded.status,
            status_category    = excluded.status_category,
            assignee_id        = excluded.assignee_id,
            assignee_name      = excluded.assignee_name,
            issue_type         = excluded.issue_type,
            raw_json           = excluded.raw_json,
            remote_updated_at  = excluded.remote_updated_at,
            sync_status        = 'synced',
            synced_at          = datetime('now')
        WHERE excluded.remote_updated_at > issues.remote_updated_at
        "#,
        issue.id,
        issue.key,
        issue.summary,
        issue.description_text,
        issue.status,
        issue.status_category,
        issue.assignee_id,
        issue.assignee_name,
        issue.project_key,
        issue.issue_type,
        issue.raw_json,
        issue.remote_updated_at,
    )
    .execute(executor)
    .await?;
    Ok(written.rows_affected() > 0)
}

/// The key the cache holds the issue under, if it has it.
pub(crate) async fn key_of(
    executor: impl sqlx::SqliteExecutor<'_>,
    id: &str,
) -> sqlx::Result<Option<IssueKey>> {
    sqlx::query_scalar("SELECT key FROM issues WHERE id = ?")
        .bind(id)
        .fetch_optional(executor)
        .await
}

pub(crate) async fn get(pool: &SqlitePool, key: &IssueKey) -> sqlx::Result<Option<IssueRow>> {
    sqlx::query_as!(
        IssueRow,
        r#"
        SELECT id AS "id!", key AS "key: IssueKey", summary, status,
               status_category AS "status_category: StatusCategory",
               assignee_name, project_key,
               sync_status AS "sync_status: SyncStatus"
        FROM issues
        WHERE key = ?
        "#,
        key
    )
    .fetch_optional(pool)
    .await
}

/// `site_url` is the Jira site the cache was filled from.
pub(crate) async fn get_detail(
    pool: &SqlitePool,
    key: &IssueKey,
    site_url: &str,
) -> sqlx::Result<Option<IssueDetail>> {
    let row = sqlx::query!(
        r#"
        SELECT key AS "key: IssueKey", summary, status,
               status_category AS "status_category: StatusCategory",
               sync_status AS "sync_status: SyncStatus",
               assignee_name, issue_type, raw_json
        FROM issues
        WHERE key = ?
        "#,
        key
    )
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else { return Ok(None) };

    // The columns win over `raw_json` where both have a value: a local change
    // updates the columns before Jira confirms it. The rest has no column.
    let raw: Value = serde_json::from_str(&row.raw_json).unwrap_or_default();
    let fields = &raw["fields"];
    let name = |value: &Value| value.as_str().map(str::to_owned);
    let url = format!("{site_url}/browse/{}", row.key);
    let comments = fields["comment"]["comments"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    let pending = pending_comments(pool, &row.key, &url).await?;
    Ok(Some(IssueDetail {
        summary: row.summary,
        status: row.status,
        status_category: row.status_category,
        sync_status: row.sync_status,
        issue_type: row.issue_type,
        assignee_name: row.assignee_name,
        reporter_name: name(&fields["reporter"]["displayName"]),
        priority: name(&fields["priority"]["name"]),
        labels: fields["labels"]
            .as_array()
            .map(|labels| labels.iter().filter_map(name).collect())
            .unwrap_or_default(),
        description_md: adf::to_markdown(fields.get("description"), &url),
        comment_total: fields["comment"]["total"]
            .as_u64()
            .map_or(comments.len(), |total| total as usize)
            + pending.len(),
        comments: comments
            .iter()
            .map(|comment| Comment {
                author_name: name(&comment["author"]["displayName"]).unwrap_or_default(),
                created: name(&comment["created"]).unwrap_or_default(),
                body_md: adf::to_markdown(comment.get("body"), &url),
                pending: false,
                rejected: None,
            })
            .chain(pending)
            .collect(),
        key: row.key,
        url,
    }))
}

/// The comments of the issue that are not in Jira: those that wait in the
/// outbox and those Jira refused, oldest first.
async fn pending_comments(
    pool: &SqlitePool,
    key: &IssueKey,
    issue_url: &str,
) -> sqlx::Result<Vec<Comment>> {
    let entries: Vec<(i64, String, String, Option<String>)> = sqlx::query_as(
        "SELECT id, payload, status, error_msg FROM sync_queue
         WHERE issue_key = ? AND entity_type = 'comment'
           AND status IN ('pending', 'in_progress', 'failed')
         ORDER BY id",
    )
    .bind(key)
    .fetch_all(pool)
    .await?;
    Ok(entries
        .into_iter()
        .map(|(entry_id, payload, status, error)| {
            let payload: Value = serde_json::from_str(&payload).unwrap_or_default();
            let text = payload["text"].as_str().unwrap_or_default();
            let failed = status == "failed";
            Comment {
                author_name: "You".to_owned(),
                created: String::new(),
                body_md: adf::to_markdown(Some(&adf::from_plain_text(text)), issue_url),
                pending: !failed,
                rejected: failed.then(|| RejectedComment {
                    entry_id,
                    reason: error.unwrap_or_default(),
                }),
            }
        })
        .collect())
}

/// Puts a comment Jira has just created at the end of the comments the cache
/// holds for the issue. The next poll replaces the whole issue, this comment
/// included, so it is never shown twice.
pub(crate) async fn add_comment(
    conn: &mut sqlx::SqliteConnection,
    key: &IssueKey,
    comment: &Value,
) -> sqlx::Result<()> {
    let raw: Option<String> = sqlx::query_scalar("SELECT raw_json FROM issues WHERE key = ?")
        .bind(key)
        .fetch_optional(&mut *conn)
        .await?;
    let Some(raw) = raw else { return Ok(()) };
    let mut raw: Value = serde_json::from_str(&raw).unwrap_or_default();
    let thread = &mut raw["fields"]["comment"];
    let mut comments = thread["comments"].as_array().cloned().unwrap_or_default();
    comments.push(comment.clone());
    // The total can be more than the comments Jira sent
    let total = thread["total"]
        .as_u64()
        .map_or(comments.len(), |total| total as usize + 1);
    thread["comments"] = Value::Array(comments);
    thread["total"] = total.into();
    sqlx::query("UPDATE issues SET raw_json = ? WHERE key = ?")
        .bind(raw.to_string())
        .bind(key)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Writes a status the user chose, which Jira has not confirmed yet. Returns
/// whether the cache has the issue.
pub(crate) async fn set_pending_status(
    executor: impl sqlx::SqliteExecutor<'_>,
    key: &IssueKey,
    status: &str,
    status_category: StatusCategory,
) -> sqlx::Result<bool> {
    let written = sqlx::query!(
        "UPDATE issues SET status = ?, status_category = ?, sync_status = 'pending' WHERE key = ?",
        status,
        status_category,
        key,
    )
    .execute(executor)
    .await?;
    Ok(written.rows_affected() > 0)
}

/// Marks the issue as confirmed by Jira, unless the outbox still holds a
/// change of it that has to be sent.
pub(crate) async fn confirm(
    executor: impl sqlx::SqliteExecutor<'_>,
    key: &IssueKey,
) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE issues SET sync_status = 'synced'
         WHERE key = ? AND sync_status = 'pending' AND NOT EXISTS (
             SELECT 1 FROM sync_queue
             WHERE issue_key = issues.key AND status IN ('pending', 'in_progress')
         )",
        key,
    )
    .execute(executor)
    .await?;
    Ok(())
}

/// Lets the next upsert of the issue overwrite the row, whatever `updated` it
/// carries: the issue is read again to undo a local change, and Jira has not
/// touched it since the row was written. Returns whether the cache has the issue.
pub(crate) async fn mark_stale(
    executor: impl sqlx::SqliteExecutor<'_>,
    id: &str,
) -> sqlx::Result<bool> {
    let marked = sqlx::query!("UPDATE issues SET remote_updated_at = '' WHERE id = ?", id)
        .execute(executor)
        .await?;
    Ok(marked.rows_affected() > 0)
}

/// What identifies a cached issue, on Jira's side and on ours.
pub(crate) struct CachedIssue {
    pub id: String,
    pub key: IssueKey,
    pub project_key: String,
}

pub(crate) async fn cached(
    executor: impl sqlx::SqliteExecutor<'_>,
) -> sqlx::Result<Vec<CachedIssue>> {
    sqlx::query_as!(
        CachedIssue,
        r#"SELECT id AS "id!", key AS "key: IssueKey", project_key FROM issues"#
    )
    .fetch_all(executor)
    .await
}

/// `id` is Jira's issue id. The FTS trigger takes the issue out of the search index.
pub(crate) async fn delete(executor: impl sqlx::SqliteExecutor<'_>, id: &str) -> sqlx::Result<()> {
    sqlx::query!("DELETE FROM issues WHERE id = ?", id)
        .execute(executor)
        .await?;
    Ok(())
}

/// Every cached issue, the one Jira changed last first.
pub(crate) async fn list(
    pool: &SqlitePool,
    project_filter: Option<&str>,
) -> sqlx::Result<Vec<IssueRow>> {
    sqlx::query_as!(
        IssueRow,
        r#"
        SELECT id AS "id!", key AS "key: IssueKey", summary, status,
               status_category AS "status_category: StatusCategory",
               assignee_name, project_key,
               sync_status AS "sync_status: SyncStatus"
        FROM issues
        WHERE ? IS NULL OR project_key = ?
        -- Both columns come from idx_issues_updated, so nothing is sorted
        ORDER BY remote_updated_at DESC, rowid DESC
        "#,
        project_filter,
        project_filter
    )
    .fetch_all(pool)
    .await
}

/// Full-text search over key, summary and description, best match first.
/// Every word of `query` must match, as a whole word, a stem or a prefix.
pub(crate) async fn search(
    pool: &SqlitePool,
    query: &str,
    project_filter: Option<&str>,
    limit: i64,
) -> sqlx::Result<Vec<IssueRow>> {
    let Some(fts_query) = match_expression(query) else {
        return Ok(Vec::new());
    };

    sqlx::query_as!(
        IssueRow,
        r#"
        SELECT i.id AS "id!", i.key AS "key: IssueKey", i.summary, i.status,
               i.status_category AS "status_category: StatusCategory",
               i.assignee_name, i.project_key,
               i.sync_status AS "sync_status: SyncStatus"
        FROM issues i
        JOIN issues_fts ON i.rowid = issues_fts.rowid
        WHERE issues_fts MATCH ?
          AND (? IS NULL OR i.project_key = ?)
        ORDER BY issues_fts.rank
        LIMIT ?
        "#,
        fts_query,
        project_filter,
        project_filter,
        limit
    )
    .fetch_all(pool)
    .await
}

// User input is not FTS5 syntax. Each word becomes a quoted string, so `"`, `*`,
// `:`, `-`, AND/OR/NOT/NEAR are plain text, and gets a `*` to match by prefix.
// A word without a letter or digit yields no token and FTS5 rejects an empty
// expression, so such words are dropped.
fn match_expression(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split_whitespace()
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .map(|word| format!("\"{}\"*", word.replace('"', "\"\"")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(id: &str, key: &str, updated_at: &str, summary: &str) -> IssueRecord {
        IssueRecord {
            id: id.to_owned(),
            key: key.into(),
            summary: summary.to_owned(),
            description_text: None,
            status: "To Do".to_owned(),
            status_category: StatusCategory::Todo,
            assignee_id: None,
            assignee_name: None,
            project_key: key.split('-').next().unwrap().to_owned(),
            issue_type: Some("Task".to_owned()),
            raw_json: "{}".to_owned(),
            remote_updated_at: JiraTimestamp::parse(updated_at).unwrap(),
        }
    }

    const T1: &str = "2026-01-01T10:00:00.000+0000";
    const T2: &str = "2026-01-02T10:00:00.000+0000";

    async fn seed(pool: &SqlitePool, issues: &[(&str, &str)]) {
        for (n, (key, summary)) in issues.iter().enumerate() {
            upsert(pool, &issue(&format!("id-{n}"), key, T1, summary))
                .await
                .unwrap();
        }
    }

    async fn found(pool: &SqlitePool, query: &str, project: Option<&str>) -> Vec<String> {
        let rows = search(pool, query, project, 10).await.unwrap();
        rows.iter().map(|row| row.key.to_string()).collect()
    }

    #[sqlx::test]
    async fn upsert_ignores_stale_remote(pool: SqlitePool) {
        upsert(&pool, &issue("1", "PROJ-1", T2, "current title"))
            .await
            .unwrap();
        upsert(&pool, &issue("1", "PROJ-1", T1, "stale title"))
            .await
            .unwrap();

        let row = get(&pool, &"PROJ-1".into()).await.unwrap().unwrap();
        assert_eq!(row.summary, "current title");
        assert_eq!(found(&pool, "stale", None).await, [] as [&str; 0]);
    }

    #[sqlx::test]
    async fn upsert_applies_newer_remote(pool: SqlitePool) {
        upsert(&pool, &issue("1", "PROJ-1", T1, "old title"))
            .await
            .unwrap();
        let newer = IssueRecord {
            status: "Done".to_owned(),
            status_category: StatusCategory::Done,
            ..issue("1", "PROJ-1", T2, "new title")
        };
        upsert(&pool, &newer).await.unwrap();

        let row = get(&pool, &"PROJ-1".into()).await.unwrap().unwrap();
        assert_eq!(row.summary, "new title");
        assert_eq!(row.status_category, StatusCategory::Done);
        assert_eq!(row.sync_status, SyncStatus::Synced);
    }

    #[sqlx::test]
    async fn upsert_applies_later_instant_whose_text_sorts_lower(pool: SqlitePool) {
        // 09:00 UTC, then 09:30 UTC written with another offset
        upsert(
            &pool,
            &issue("1", "PROJ-1", "2026-01-01T10:00:00.000+0100", "old title"),
        )
        .await
        .unwrap();
        upsert(
            &pool,
            &issue("1", "PROJ-1", "2026-01-01T04:30:00.000-0500", "new title"),
        )
        .await
        .unwrap();

        let row = get(&pool, &"PROJ-1".into()).await.unwrap().unwrap();
        assert_eq!(row.summary, "new title");
    }

    #[sqlx::test]
    async fn upsert_ignores_earlier_instant_whose_text_sorts_higher(pool: SqlitePool) {
        // 09:30 UTC, then 09:00 UTC written with another offset
        upsert(
            &pool,
            &issue(
                "1",
                "PROJ-1",
                "2026-01-01T04:30:00.000-0500",
                "current title",
            ),
        )
        .await
        .unwrap();
        upsert(
            &pool,
            &issue("1", "PROJ-1", "2026-01-01T10:00:00.000+0100", "stale title"),
        )
        .await
        .unwrap();

        let row = get(&pool, &"PROJ-1".into()).await.unwrap().unwrap();
        assert_eq!(row.summary, "current title");
    }

    #[sqlx::test]
    async fn upsert_orders_instants_across_a_daylight_saving_change(pool: SqlitePool) {
        // Clocks go back from +0200 to +0100 on 2026-10-25 at 01:00 UTC, so the
        // local hour 02:00-03:00 happens twice and the second pass sorts lower
        upsert(
            &pool,
            &issue("1", "PROJ-1", "2026-10-25T02:50:00.000+0200", "before"),
        )
        .await
        .unwrap();
        upsert(
            &pool,
            &issue("1", "PROJ-1", "2026-10-25T02:10:00.000+0100", "after"),
        )
        .await
        .unwrap();
        upsert(
            &pool,
            &issue("1", "PROJ-1", "2026-10-25T02:50:00.000+0200", "before"),
        )
        .await
        .unwrap();

        let row = get(&pool, &"PROJ-1".into()).await.unwrap().unwrap();
        assert_eq!(row.summary, "after");
    }

    #[sqlx::test]
    async fn remote_updated_at_is_stored_in_utc(pool: SqlitePool) {
        upsert(
            &pool,
            &issue("1", "PROJ-1", "2026-03-29T01:30:00.000+0100", "a"),
        )
        .await
        .unwrap();
        upsert(
            &pool,
            &issue("2", "PROJ-2", "2026-03-29T03:30:00.5+0200", "b"),
        )
        .await
        .unwrap();

        let stored: Vec<String> =
            sqlx::query_scalar("SELECT remote_updated_at FROM issues ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            stored,
            ["2026-03-29T00:30:00.000Z", "2026-03-29T01:30:00.500Z"]
        );
    }

    #[test]
    fn jira_timestamp_rejects_what_is_not_a_timestamp() {
        for value in [
            "",
            "yesterday",
            "2026-01-01",
            "2026-01-01T10:00:00.000",
            "2026-13-01T10:00:00.000+0000",
        ] {
            assert!(
                JiraTimestamp::parse(value).is_err(),
                "{value:?} was accepted"
            );
        }
    }

    #[sqlx::test]
    async fn list_returns_every_issue_with_the_last_changed_first(pool: SqlitePool) {
        upsert(&pool, &issue("1", "PROJ-1", T1, "Older"))
            .await
            .unwrap();
        upsert(&pool, &issue("2", "PROJ-2", T2, "Newer"))
            .await
            .unwrap();
        upsert(&pool, &issue("3", "PROJ-3", T1, "Older, stored later"))
            .await
            .unwrap();

        let rows = list(&pool, None).await.unwrap();
        let keys: Vec<String> = rows.iter().map(|row| row.key.to_string()).collect();
        assert_eq!(keys, ["PROJ-2", "PROJ-3", "PROJ-1"]);
    }

    #[sqlx::test]
    async fn list_can_be_narrowed_to_one_project(pool: SqlitePool) {
        upsert(&pool, &issue("1", "PROJ-1", T1, "Older"))
            .await
            .unwrap();
        upsert(&pool, &issue("2", "OTHER-1", T2, "Newer"))
            .await
            .unwrap();
        upsert(&pool, &issue("3", "PROJ-2", T2, "Newest"))
            .await
            .unwrap();

        let rows = list(&pool, Some("PROJ")).await.unwrap();
        let keys: Vec<String> = rows.iter().map(|row| row.key.to_string()).collect();
        assert_eq!(keys, ["PROJ-2", "PROJ-1"]);
    }

    #[sqlx::test]
    async fn upsert_follows_moved_issue(pool: SqlitePool) {
        upsert(&pool, &issue("1", "PROJ-1", T1, "title"))
            .await
            .unwrap();
        // Same Jira id, new key: the issue was moved to another watched project
        upsert(&pool, &issue("1", "OTHER-7", T2, "title"))
            .await
            .unwrap();

        assert!(get(&pool, &"PROJ-1".into()).await.unwrap().is_none());
        let moved = get(&pool, &"OTHER-7".into()).await.unwrap().unwrap();
        assert_eq!(moved.project_key, "OTHER");
        let count = sqlx::query_scalar!("SELECT COUNT(*) FROM issues")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[sqlx::test]
    async fn fts_index_follows_insert_update_and_delete(pool: SqlitePool) {
        upsert(&pool, &issue("1", "PROJ-1", T1, "Broken login"))
            .await
            .unwrap();
        assert_eq!(found(&pool, "login", None).await, ["PROJ-1"]);

        upsert(&pool, &issue("1", "PROJ-1", T2, "Broken checkout"))
            .await
            .unwrap();
        assert_eq!(found(&pool, "login", None).await, [] as [&str; 0]);
        assert_eq!(found(&pool, "checkout", None).await, ["PROJ-1"]);

        sqlx::query!("DELETE FROM issues WHERE id = '1'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(found(&pool, "checkout", None).await, [] as [&str; 0]);
        check_fts_integrity(&pool).await;
    }

    async fn check_fts_integrity(pool: &SqlitePool) {
        // Fails if the index and the content table disagree
        sqlx::query("INSERT INTO issues_fts(issues_fts, rank) VALUES ('integrity-check', 1)")
            .execute(pool)
            .await
            .unwrap();
    }

    #[sqlx::test]
    async fn search_survives_vacuum(pool: SqlitePool) {
        seed(
            &pool,
            &[
                ("PROJ-1", "Alpha"),
                ("PROJ-2", "Bravo"),
                ("PROJ-3", "Charlie"),
            ],
        )
        .await;
        sqlx::query("DELETE FROM issues WHERE id = 'id-0'")
            .execute(&pool)
            .await
            .unwrap();

        // SQLite only promises to keep rowids over VACUUM for an INTEGER PRIMARY
        // KEY. Current versions keep implicit ones too, so the search below
        // cannot catch a regression of the schema; this check does.
        let rowid_type: String =
            sqlx::query_scalar("SELECT type FROM pragma_table_info('issues') WHERE pk = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(rowid_type, "INTEGER");

        sqlx::query("VACUUM").execute(&pool).await.unwrap();

        assert_eq!(found(&pool, "bravo", None).await, ["PROJ-2"]);
        assert_eq!(found(&pool, "charlie", None).await, ["PROJ-3"]);
        check_fts_integrity(&pool).await;
    }

    #[sqlx::test(migrations = false)]
    async fn rowid_migration_keeps_the_rows_of_an_older_database(pool: SqlitePool) {
        for migration in [
            include_str!("../../migrations/0001_initial.sql"),
            include_str!("../../migrations/0002_fts5.sql"),
        ] {
            sqlx::raw_sql(migration).execute(&pool).await.unwrap();
        }
        for (id, key, summary) in [
            ("10", "PROJ-1", "Alpha"),
            ("11", "PROJ-2", "Bravo"),
            ("12", "PROJ-3", "Charlie"),
        ] {
            sqlx::query(
                "INSERT INTO issues (id, key, summary, status, status_category, project_key,
                                     raw_json, sync_status, remote_updated_at)
                 VALUES (?, ?, ?, 'To Do', 'todo', 'PROJ', '{}', 'pending', '2026-01-01T10:00:00.000Z')",
            )
            .bind(id)
            .bind(key)
            .bind(summary)
            .execute(&pool)
            .await
            .unwrap();
        }
        // Leaves a gap in the rowids, as a real cache has
        sqlx::query("DELETE FROM issues WHERE id = '10'")
            .execute(&pool)
            .await
            .unwrap();

        sqlx::raw_sql(include_str!("../../migrations/0003_issues_rowid.sql"))
            .execute(&pool)
            .await
            .unwrap();

        let row = get(&pool, &"PROJ-3".into()).await.unwrap().unwrap();
        assert_eq!(row.id, "12");
        assert_eq!(row.summary, "Charlie");
        assert_eq!(row.sync_status, SyncStatus::Pending);
        assert!(get(&pool, &"PROJ-2".into()).await.unwrap().is_some());
        assert!(get(&pool, &"PROJ-1".into()).await.unwrap().is_none());
        assert_eq!(found(&pool, "bravo", None).await, ["PROJ-2"]);
        assert_eq!(found(&pool, "charlie", None).await, ["PROJ-3"]);
        check_fts_integrity(&pool).await;

        // The upsert still finds the row by its Jira id
        upsert(&pool, &issue("12", "PROJ-3", T2, "Delta"))
            .await
            .unwrap();
        assert_eq!(found(&pool, "delta", None).await, ["PROJ-3"]);
        assert_eq!(found(&pool, "charlie", None).await, [] as [&str; 0]);
    }

    #[sqlx::test]
    async fn search_matches_by_prefix(pool: SqlitePool) {
        seed(&pool, &[("PROJ-1", "Fix authentication failure")]).await;

        assert_eq!(found(&pool, "auth", None).await, ["PROJ-1"]);
        assert_eq!(found(&pool, "fix fail", None).await, ["PROJ-1"]);
        assert_eq!(found(&pool, "fix payment", None).await, [] as [&str; 0]);
    }

    #[sqlx::test]
    async fn search_matches_by_porter_stem(pool: SqlitePool) {
        seed(&pool, &[("PROJ-1", "Fix authentication failure")]).await;

        assert_eq!(found(&pool, "authenticating", None).await, ["PROJ-1"]);
    }

    #[sqlx::test]
    async fn search_matches_by_issue_key_and_description(pool: SqlitePool) {
        seed(&pool, &[("PROJ-1", "First"), ("PROJ-12", "Second")]).await;
        let described = IssueRecord {
            description_text: Some("Stack trace attached".to_owned()),
            ..issue("d", "PROJ-3", T1, "Third")
        };
        upsert(&pool, &described).await.unwrap();

        let mut by_key = found(&pool, "PROJ-1", None).await;
        by_key.sort();
        assert_eq!(by_key, ["PROJ-1", "PROJ-12"]);
        assert_eq!(found(&pool, "trace", None).await, ["PROJ-3"]);
    }

    #[sqlx::test]
    async fn search_can_be_limited_to_one_project(pool: SqlitePool) {
        seed(&pool, &[("PROJ-1", "Login page"), ("OTHER-1", "Login API")]).await;

        assert_eq!(found(&pool, "login", None).await.len(), 2);
        assert_eq!(found(&pool, "login", Some("OTHER")).await, ["OTHER-1"]);
    }

    #[sqlx::test]
    async fn search_treats_quotes_and_fts5_operators_as_text(pool: SqlitePool) {
        seed(
            &pool,
            &[("PROJ-1", "Login AND logout near the \"home\" page")],
        )
        .await;

        for query in [
            "\"",
            "\"home",
            "say \"home\" page",
            "login AND",
            "AND",
            "OR login",
            "NOT",
            "NEAR(login logout)",
            "login*",
            "*",
            "summary:login",
            "key : PROJ",
            "-login",
            "^login",
            "(login",
            "login)",
            "{login logout}",
            "lo+gin",
            "'; DROP TABLE issues; --",
            "",
            "   ",
        ] {
            search(&pool, query, None, 10)
                .await
                .unwrap_or_else(|err| panic!("query {query:?} failed: {err}"));
        }

        assert_eq!(found(&pool, "\"home\"", None).await, ["PROJ-1"]);
        assert_eq!(found(&pool, "login AND", None).await, ["PROJ-1"]);
        assert_eq!(
            found(&pool, "login NOT logout", None).await,
            [] as [&str; 0]
        );
    }
}
