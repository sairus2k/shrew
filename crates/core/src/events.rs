use crate::models::IssueKey;

#[derive(Clone, Debug)]
pub enum SyncEvent {
    Started,
    Completed,
    // Keys of issues whose local rows changed, by a local write or by the sync engine
    IssuesUpdated(Vec<IssueKey>),
    InitialSyncComplete,
    Error(String),
}
