//! The outbox: local changes that Jira has not confirmed yet.

use crate::models::IssueKey;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::SqlitePool;

/// What an outbox entry changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EntryKind {
    Transition,
    Assignee,
    Comment,
    Worklog,
}

impl EntryKind {
    // Stored in `entity_type` and compared in the SQL of `db`: the entries an
    // install has queued keep these names
    fn name(self) -> &'static str {
        match self {
            Self::Transition => "transition",
            Self::Assignee => "assignee",
            Self::Comment => "comment",
            Self::Worklog => "worklog",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "transition" => Some(Self::Transition),
            "assignee" => Some(Self::Assignee),
            "comment" => Some(Self::Comment),
            "worklog" => Some(Self::Worklog),
            _ => None,
        }
    }
}

/// A change of the user's as the outbox holds it until Jira has it. The
/// payload of each kind is stored as JSON in the shape the entries an install
/// has queued already have.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Change {
    Transition(TransitionChange),
    Assignee(AssigneeChange),
    Comment(CommentChange),
    Worklog(WorklogChange),
}

/// The body of Jira's transition request, which is sent as it is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct TransitionChange {
    pub transition: TransitionId,
    /// The values of the transition's required fields, by field id.
    pub fields: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct TransitionId {
    pub id: String,
}

/// The body of Jira's assign request, which is sent as it is: `None` is sent
/// as `null` and leaves the issue unassigned.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct AssigneeChange {
    #[serde(rename = "accountId")]
    pub account_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct CommentChange {
    pub text: String,
    /// The id of the comment that heads the thread this one answers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
}

/// What the user entered: the target is chosen at delivery.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct WorklogChange {
    // `db::worklogs` finds the entry of a worklog by `$.local_id` in SQL
    pub local_id: String,
    pub time_spent_seconds: i64,
    pub started_at: String,
    pub comment: Option<String>,
}

impl Change {
    pub(crate) fn kind(&self) -> EntryKind {
        match self {
            Self::Transition(_) => EntryKind::Transition,
            Self::Assignee(_) => EntryKind::Assignee,
            Self::Comment(_) => EntryKind::Comment,
            Self::Worklog(_) => EntryKind::Worklog,
        }
    }

    fn payload(&self) -> String {
        match self {
            Self::Transition(change) => serde_json::to_string(change),
            Self::Assignee(change) => serde_json::to_string(change),
            Self::Comment(change) => serde_json::to_string(change),
            Self::Worklog(change) => serde_json::to_string(change),
        }
        // Strings, numbers and JSON values only, which always serialize
        .expect("an outbox payload serializes")
    }

    fn read(entity_type: &str, payload: &str) -> Result<Self, Unreadable> {
        let kind = EntryKind::from_name(entity_type).ok_or_else(|| Unreadable {
            kind: None,
            reason: format!("this version of Shrew does not know the change `{entity_type}`"),
        })?;
        match kind {
            EntryKind::Transition => serde_json::from_str(payload).map(Self::Transition),
            EntryKind::Assignee => serde_json::from_str(payload).map(Self::Assignee),
            EntryKind::Comment => serde_json::from_str(payload).map(Self::Comment),
            EntryKind::Worklog => serde_json::from_str(payload).map(Self::Worklog),
        }
        .map_err(|error| Unreadable {
            kind: Some(kind),
            reason: format!("the stored change cannot be read: {error}"),
        })
    }
}

/// An entry this version cannot deliver: one of a kind it does not know, as a
/// newer version may have queued, or one whose payload is damaged.
#[derive(Debug)]
pub(crate) struct Unreadable {
    pub kind: Option<EntryKind>,
    pub reason: String,
}

pub(crate) struct QueueEntry {
    pub id: i64,
    pub issue_key: IssueKey,
    pub change: Result<Change, Unreadable>,
    pub retry_count: i64,
}

impl QueueEntry {
    pub(crate) fn kind(&self) -> Option<EntryKind> {
        match &self.change {
            Ok(change) => Some(change.kind()),
            Err(unreadable) => unreadable.kind,
        }
    }
}

/// Adds a change to send to Jira.
pub(crate) async fn enqueue(
    executor: impl sqlx::SqliteExecutor<'_>,
    issue_key: &IssueKey,
    change: &Change,
) -> sqlx::Result<()> {
    let entity_type = change.kind().name();
    let payload = change.payload();
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
    let row = sqlx::query!(
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
    .await?;
    Ok(row.map(|row| QueueEntry {
        id: row.id,
        issue_key: row.issue_key,
        change: Change::read(&row.entity_type, &row.payload),
        retry_count: row.retry_count,
    }))
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
/// Returns the kind of each, `None` for one this version does not know.
pub(crate) async fn remove_for_issue(
    executor: impl sqlx::SqliteExecutor<'_>,
    key: &IssueKey,
) -> sqlx::Result<Vec<Option<EntryKind>>> {
    let names = sqlx::query_scalar!(
        "DELETE FROM sync_queue WHERE issue_key = ? RETURNING entity_type",
        key
    )
    .fetch_all(executor)
    .await?;
    Ok(names
        .iter()
        .map(|name| EntryKind::from_name(name))
        .collect())
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_kind_is_read_back_under_its_stored_name() {
        for kind in [
            EntryKind::Transition,
            EntryKind::Assignee,
            EntryKind::Comment,
            EntryKind::Worklog,
        ] {
            assert_eq!(EntryKind::from_name(kind.name()), Some(kind));
        }
    }

    /// The payloads as the versions before the typed outbox wrote them.
    #[test]
    fn the_entries_an_install_has_queued_are_read_and_written_unchanged() {
        let worklog = json!({
            "local_id": "9f2c41d07a3b8e65",
            "time_spent_seconds": 90,
            "started_at": "2026-03-29T08:00:00.000Z",
            "comment": null,
        });
        let mut described_worklog = worklog.clone();
        described_worklog["comment"] = "Review".into();
        let stored = [
            (
                "transition",
                json!({ "transition": { "id": "21" }, "fields": { "resolution": { "name": "Done" } } }),
            ),
            (
                "transition",
                json!({ "transition": { "id": "11" }, "fields": {} }),
            ),
            ("assignee", json!({ "accountId": "5b10a" })),
            ("assignee", json!({ "accountId": null })),
            ("comment", json!({ "text": "Seen on staging" })),
            ("comment", json!({ "text": "Agreed", "parent_id": "10001" })),
            ("worklog", worklog),
            ("worklog", described_worklog),
        ];
        for (entity_type, payload) in stored {
            let change = Change::read(entity_type, &payload.to_string()).unwrap();
            assert_eq!(change.kind().name(), entity_type);
            let written: Value = serde_json::from_str(&change.payload()).unwrap();
            assert_eq!(written, payload);
        }
    }

    #[test]
    fn an_unknown_kind_or_a_payload_without_the_shape_of_its_kind_is_unreadable() {
        let unknown = Change::read("label", r#"{"name":"urgent"}"#).unwrap_err();
        assert_eq!(unknown.kind, None);
        for payload in ["not json", r#"{"time_spent_seconds":90}"#] {
            let damaged = Change::read("worklog", payload).unwrap_err();
            assert_eq!(damaged.kind, Some(EntryKind::Worklog));
        }
    }
}
