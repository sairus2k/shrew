use crate::{db::sync_queue::EntryKind, models::IssueKey};

#[derive(Clone, Debug)]
pub enum SyncEvent {
    Started,
    Completed,
    // Keys of issues whose local rows changed, by a local write or by the sync engine
    IssuesUpdated(Vec<IssueKey>),
    // Sent before the first page of a project and after every stored page
    InitialSyncProgress {
        project_key: String,
        synced: u64,
        // Jira's estimate: `synced` can end below or above it
        approximate_total: u64,
    },
    InitialSyncComplete,
    // The sync could not read this issue and went on without it
    IssueSkipped {
        key: String,
        reason: String,
    },
    // Jira answered 401: the sync is paused until `api::renew_token` succeeds
    TokenRejected,
    // Tempo answered 401 to a worklog: the worklogs stay queued until
    // `api::save_tempo_token` saves another token or the target is Jira again
    TempoTokenRejected,
    // A change made here will never reach Jira: Jira refused it, or the issue it
    // belongs to is gone. `action` names it for the user, such as "status change".
    ChangeRejected {
        key: IssueKey,
        action: String,
        reason: String,
    },
    Error(String),
}

/// What the user is told about an entry that will not reach Jira. `kind` is
/// `None` for an entry of a kind this version does not know.
pub(crate) fn rejection(key: &IssueKey, kind: Option<EntryKind>, reason: &str) -> SyncEvent {
    let action = match kind {
        Some(EntryKind::Transition) => "status change",
        Some(EntryKind::Assignee) => "assignee change",
        Some(EntryKind::Comment) => "comment",
        Some(EntryKind::Worklog) => "worklog",
        None => "change",
    };
    SyncEvent::ChangeRejected {
        key: key.clone(),
        action: action.to_owned(),
        reason: reason.to_owned(),
    }
}
