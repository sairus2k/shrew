use crate::models::IssueKey;

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
