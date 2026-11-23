//! Reading a Jira issue into the cache record, shared by both directions of the sync.

use crate::{
    adf,
    db::issues::{IssueRecord, JiraTimestamp},
    models::StatusCategory,
    sync::jira_client::JiraClient,
};
use anyhow::Context;
use serde::Deserialize;
use serde_json::Value;

pub(crate) const ISSUE_FIELDS: &[&str] = &[
    "summary",
    "description",
    "status",
    "assignee",
    "reporter",
    "priority",
    "labels",
    "comment",
    "project",
    "issuetype",
    "updated",
];

#[derive(Deserialize)]
struct JiraIssue {
    id: String,
    key: String,
    fields: Fields,
}

#[derive(Deserialize)]
struct Fields {
    summary: String,
    description: Option<Value>,
    status: Status,
    assignee: Option<User>,
    project: ProjectRef,
    issuetype: Option<IssueType>,
    priority: Option<Priority>,
    updated: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    name: String,
    status_category: Category,
}

#[derive(Deserialize)]
struct Category {
    key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct User {
    account_id: String,
    display_name: String,
}

#[derive(Deserialize)]
struct ProjectRef {
    key: String,
}

#[derive(Deserialize)]
struct IssueType {
    name: String,
}

#[derive(Deserialize)]
struct Priority {
    name: String,
}

/// `issue` is one element of a search page, fetched with `ISSUE_FIELDS`.
pub(crate) fn issue_record(issue: &Value) -> anyhow::Result<IssueRecord> {
    let JiraIssue { id, key, fields } = JiraIssue::deserialize(issue)
        .with_context(|| format!("unexpected issue from Jira: {}", issue["key"]))?;
    let description_text = adf::to_plain_text(fields.description.as_ref());
    let (assignee_id, assignee_name) = fields
        .assignee
        .map(|user| (user.account_id, user.display_name))
        .unzip();
    Ok(IssueRecord {
        id,
        key: key.as_str().into(),
        summary: fields.summary,
        description_text: (!description_text.is_empty()).then_some(description_text),
        status: fields.status.name,
        status_category: status_category(&fields.status.status_category.key),
        assignee_id,
        assignee_name,
        project_key: fields.project.key,
        issue_type: fields.issuetype.map(|issue_type| issue_type.name),
        priority: fields.priority.map(|priority| priority.name),
        raw_json: issue.to_string(),
        // chrono is built without `std`, so its error is not a `std::error::Error`
        remote_updated_at: JiraTimestamp::parse(&fields.updated)
            .map_err(|error| anyhow::anyhow!("{key} has an unreadable `updated`: {error}"))?,
    })
}

/// Writes into each comment of `issues` that is a reply the `parentId` of the
/// comment it answers, as the comments of one issue have it. Jira is asked
/// once for all of them, and not at all when no issue has a comment.
pub(crate) async fn mark_replies(client: &JiraClient, issues: &mut [Value]) -> anyhow::Result<()> {
    const COMMENTS: &str = "/fields/comment/comments";
    let ids: Vec<i64> = issues
        .iter()
        .filter_map(|issue| issue.pointer(COMMENTS)?.as_array())
        .flatten()
        .filter_map(|comment| comment["id"].as_str()?.parse().ok())
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    let parents = client.comment_parents(&ids).await?;
    let comments = issues
        .iter_mut()
        .filter_map(|issue| issue.pointer_mut(COMMENTS)?.as_array_mut())
        .flatten();
    for comment in comments {
        let parent = comment["id"].as_str().and_then(|id| parents.get(id));
        if let Some(parent) = parent {
            comment["parentId"] = Value::String(parent.clone());
        }
    }
    Ok(())
}

/// `key` is the key of a status category in Jira.
pub(crate) fn status_category(key: &str) -> StatusCategory {
    match key {
        "indeterminate" => StatusCategory::InProgress,
        "done" => StatusCategory::Done,
        // "new", and "undefined" for a status with no category
        _ => StatusCategory::Todo,
    }
}
