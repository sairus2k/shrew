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
    // Jira answered 401: the sync is paused until `api::renew_token` succeeds
    TokenRejected,
    Error(String),
}
