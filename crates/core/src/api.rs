//! The async functions the UI calls. They need the tokio runtime.

use crate::{
    AppState, SyncEvent, auth, db,
    models::{
        Account, FilterOptions, IssueDetail, IssueFilter, IssueKey, IssueRow, Project,
        RunningTimer, SetupStep, Transition, User, WorklogSettings, WorklogTarget,
    },
    sync::{
        assignable, inbound,
        jira_client::{JiraClient, JiraError},
        transitions,
    },
};
use anyhow::{Context, bail};
use serde_json::json;

/// The account the app is signed in to, or `None` when it has no credentials.
pub async fn account(state: &AppState) -> Option<Account> {
    let config = state.config.read().await;
    config.as_ref().map(|config| Account {
        site_url: config.site_url.clone(),
        email: config.email.clone(),
        account_id: config.account_id.clone(),
    })
}

/// Why `sign_in` refused: the database holds changes that Jira has not
/// received and that the account signing in did not make. `sign_in_discarding`
/// deletes them.
#[derive(Debug)]
pub struct UnsentChanges {
    pub count: i64,
    /// Who made them: signing in to this account keeps them. `None` when the
    /// database does not say.
    pub owner: Option<Account>,
}

impl std::fmt::Display for UnsentChanges {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Unsent changes of another account: {}", self.count)
    }
}

impl std::error::Error for UnsentChanges {}

/// Checks the credentials with Jira and saves them only if Jira accepts them.
/// The error says what went wrong in words meant for the user, or is an
/// `UnsentChanges`.
///
/// The account whose data the database holds keeps all of it, unsent changes
/// included: that is the sign-in after a config file was lost. Any other
/// account starts from an empty database.
pub async fn sign_in(
    state: &AppState,
    site_url: &str,
    email: &str,
    api_token: &str,
) -> anyhow::Result<Account> {
    sign_in_as(state, site_url, email, api_token, false).await
}

/// `sign_in` that deletes the unsent changes of another account instead of
/// refusing. Call it only after the user was shown the `UnsentChanges` and
/// confirmed.
pub async fn sign_in_discarding(
    state: &AppState,
    site_url: &str,
    email: &str,
    api_token: &str,
) -> anyhow::Result<Account> {
    sign_in_as(state, site_url, email, api_token, true).await
}

async fn sign_in_as(
    state: &AppState,
    site_url: &str,
    email: &str,
    api_token: &str,
    discard_unsent: bool,
) -> anyhow::Result<Account> {
    let (site_url, email, api_token) = (site_url.trim(), email.trim(), api_token.trim());
    if site_url.is_empty() || email.is_empty() || api_token.is_empty() {
        bail!("Enter the site URL, the email and the API token");
    }
    let site_url = site_url.trim_end_matches('/');
    let site_url = if site_url.contains("://") {
        site_url.to_owned()
    } else {
        format!("https://{site_url}")
    };

    let myself = match JiraClient::new(&site_url, email, api_token).myself().await {
        Ok(myself) => myself,
        Err(JiraError::Unauthorized) => bail!("Jira rejected the email or the API token"),
        Err(JiraError::Status(status, _)) => {
            bail!("{site_url} answered {status}. Check the site URL")
        }
        Err(JiraError::Http(error)) => bail!("Could not reach {site_url}: {error}"),
    };

    let config = auth::Config {
        site_url,
        email: email.to_owned(),
        account_id: myself.account_id,
        display_name: myself.display_name,
        api_token: api_token.to_owned(),
        worklog_target: Default::default(),
        shortcut_hints: true,
        tempo_api_token: String::new(),
    };
    let account = Account {
        site_url: config.site_url.clone(),
        email: config.email.clone(),
        account_id: config.account_id.clone(),
    };
    let owner = db::owner(&state.pool).await?;
    // The email is not compared: an Atlassian account can change it
    let own_data = owner.as_ref().is_some_and(|owner| {
        owner.site_url == account.site_url && owner.account_id == account.account_id
    });
    if !own_data {
        // After a sign-out the database is empty. It is not after one that
        // failed halfway, or when the config file was lost or deleted by hand.
        let count = db::sync_queue::unsent(&state.pool).await?;
        if count > 0 && !discard_unsent {
            return Err(UnsentChanges { count, owner }.into());
        }
        db::clear(&state.pool).await?;
    }
    auth::save(&state.data_dir, state.tokens.clone(), &config).await?;
    db::set_owner(&state.pool, &account).await?;
    *state.config.write().await = Some(config);
    Ok(account)
}

/// How many local changes Jira has not received yet. A sign-out discards them.
pub async fn unsent_changes(state: &AppState) -> anyhow::Result<i64> {
    Ok(db::sync_queue::unsent(&state.pool).await?)
}

/// Forgets the account: deletes the saved credentials and everything cached
/// from Jira, unsent changes included. `setup_step` is `SignIn` afterwards.
pub async fn sign_out(state: &AppState) -> anyhow::Result<()> {
    state.signed_out.send_replace(());
    let _no_sync = state.sync_running.lock().await;
    let mut config = state.config.write().await;
    auth::remove(
        &state.data_dir,
        state.tokens.clone(),
        state.tempo_tokens.clone(),
    )
    .await?;
    *config = None;
    db::clear(&state.pool).await?;
    Ok(())
}

/// Replaces the API token after `SyncEvent::TokenRejected`, or saves one at
/// `SetupStep::EnterToken`. The token is checked with Jira and saved only if
/// Jira accepts it; the sync then resumes.
/// The error says what went wrong in words meant for the user.
pub async fn renew_token(state: &AppState, api_token: &str) -> anyhow::Result<()> {
    let api_token = api_token.trim();
    if api_token.is_empty() {
        bail!("Enter the API token");
    }
    let config = state.config.read().await.clone().context("Sign in first")?;
    let site_url = &config.site_url;
    match JiraClient::new(site_url, &config.email, api_token)
        .myself()
        .await
    {
        Ok(_) => {}
        Err(JiraError::Unauthorized) => bail!("Jira rejected the API token"),
        Err(JiraError::Status(status, _)) => bail!("{site_url} answered {status}"),
        Err(JiraError::Http(error)) => bail!("Could not reach {site_url}: {error}"),
    }

    let mut saved = state.config.write().await;
    // Only the token is replaced: a sync may have moved its times meanwhile
    let current = saved.as_mut().context("Sign in first")?;
    current.api_token = api_token.to_owned();
    auth::save(&state.data_dir, state.tokens.clone(), current).await?;
    drop(saved);
    state.sync_waker.send(()).ok();
    Ok(())
}

/// Where logged time goes, and whether a Tempo token is saved.
pub async fn worklog_settings(state: &AppState) -> anyhow::Result<WorklogSettings> {
    let config = state.config.read().await;
    let config = config.as_ref().context("Sign in first")?;
    Ok(WorklogSettings {
        target: config.worklog_target,
        tempo_token_saved: !config.tempo_api_token.is_empty(),
    })
}

/// Chooses where the queued and the future worklogs are sent. Tempo needs its
/// token first.
pub async fn set_worklog_target(state: &AppState, target: WorklogTarget) -> anyhow::Result<()> {
    let mut saved = state.config.write().await;
    let current = saved.as_mut().context("Sign in first")?;
    if target == WorklogTarget::Tempo && current.tempo_api_token.is_empty() {
        bail!("Save the Tempo API token first");
    }
    current.worklog_target = target;
    auth::save_config(&state.data_dir, current).await?;
    *state.rejected_tempo_token.lock().unwrap() = None;
    drop(saved);
    state.sync_waker.send(()).ok();
    Ok(())
}

/// Whether the controls show the key of their shortcut. They do until the
/// user turns it off, and while nobody is signed in.
pub async fn shortcut_hints(state: &AppState) -> bool {
    let config = state.config.read().await;
    config.as_ref().is_none_or(|config| config.shortcut_hints)
}

pub async fn set_shortcut_hints(state: &AppState, shown: bool) -> anyhow::Result<()> {
    let mut saved = state.config.write().await;
    let current = saved.as_mut().context("Sign in first")?;
    current.shortcut_hints = shown;
    auth::save_config(&state.data_dir, current).await
}

/// Saves the Tempo API token in the token store. It is not checked: Tempo has
/// no call for that that the app needs, and a wrong one is reported as
/// `SyncEvent::TempoTokenRejected` when a worklog is sent.
pub async fn save_tempo_token(state: &AppState, api_token: &str) -> anyhow::Result<()> {
    let api_token = api_token.trim();
    if api_token.is_empty() {
        bail!("Enter the Tempo API token");
    }
    let mut saved = state.config.write().await;
    let current = saved.as_mut().context("Sign in first")?;
    let tempo_tokens = state.tempo_tokens.clone();
    let token = api_token.to_owned();
    tokio::task::spawn_blocking(move || tempo_tokens.save(&token)).await??;
    current.tempo_api_token = api_token.to_owned();
    *state.rejected_tempo_token.lock().unwrap() = None;
    drop(saved);
    state.sync_waker.send(()).ok();
    Ok(())
}

/// Starts a sync cycle without waiting for the poll interval. A request made
/// while a cycle runs is served by one more cycle after it, however many came.
pub fn sync_now(state: &AppState) {
    state.sync_waker.send(()).ok();
}

/// When the last sync that completed had started, in Unix milliseconds.
/// `None` until the initial sync has finished.
pub async fn last_sync_at(state: &AppState) -> anyhow::Result<Option<i64>> {
    Ok(db::sync_state::load(&state.pool).await?.last_sync_at)
}

pub async fn setup_step(state: &AppState) -> anyhow::Result<SetupStep> {
    match &*state.config.read().await {
        None => return Ok(SetupStep::SignIn),
        Some(config) if config.api_token.is_empty() => return Ok(SetupStep::EnterToken),
        Some(_) => {}
    }
    let sync = db::sync_state::load(&state.pool).await?;
    Ok(if sync.watched_projects.is_empty() {
        SetupStep::ChooseProjects
    } else if sync.last_sync_at.is_none() || !sync.pending_projects.is_empty() {
        SetupStep::InitialSync
    } else {
        SetupStep::Ready
    })
}

/// The projects the account can see, ordered by name.
pub async fn projects(state: &AppState) -> anyhow::Result<Vec<Project>> {
    let config = state.config.read().await.clone().context("Sign in first")?;
    let projects = JiraClient::new(&config.site_url, &config.email, &config.api_token)
        .projects()
        .await?;
    Ok(projects
        .into_iter()
        .map(|project| Project {
            key: project.key,
            name: project.name,
        })
        .collect())
}

/// Saves the projects whose issues the app keeps locally. After the initial
/// sync the added projects wait for `initial_sync` to download them, and the
/// cached issues of the removed ones are deleted, with their worklogs and
/// timer. Their queued changes are still sent to Jira: the user made them on
/// purpose. The list and the deletion are one transaction: after a failure
/// the cache holds the issues of the projects the list names.
pub async fn watch_projects(state: &AppState, project_keys: &[String]) -> anyhow::Result<()> {
    if project_keys.is_empty() {
        bail!("Choose at least one project");
    }
    // Held until the commit, as the pass for deleted issues holds it: a page
    // of a sync is stored before the delete or filtered by the new list
    let saved = state.config.write().await;
    saved.as_ref().context("Sign in first")?;

    let mut removed = Vec::new();
    let mut tx = crate::db::begin(&state.pool).await?;
    db::sync_state::watch(&mut tx, project_keys).await?;
    for issue in db::issues::cached(&mut *tx).await? {
        if !project_keys.contains(&issue.project_key) {
            db::issues::delete(&mut *tx, &issue.id).await?;
            db::worklogs::delete_for_issue(&mut *tx, &issue.key).await?;
            db::timer::delete_for_issue(&mut *tx, &issue.key).await?;
            // A refused worklog has no row left to be retried or discarded from
            db::sync_queue::remove_failed_worklogs(&mut *tx, &issue.key).await?;
            removed.push(issue.key);
        }
    }
    tx.commit().await?;
    drop(saved);
    if !removed.is_empty() {
        state.events.send(SyncEvent::IssuesUpdated(removed)).ok();
    }
    Ok(())
}

/// Downloads every issue of the watched projects, reporting through
/// `SyncEvent::InitialSyncProgress` and `SyncEvent::InitialSyncComplete`.
/// After an error or a quit, calling it again completes the sync.
pub async fn initial_sync(state: &AppState) -> anyhow::Result<()> {
    inbound::initial_sync(state).await
}

/// The projects whose issues the app keeps locally, in the order they were chosen.
pub async fn watched_projects(state: &AppState) -> anyhow::Result<Vec<String>> {
    Ok(db::sync_state::watched(&state.pool).await?)
}

/// The issues in the local cache that pass `filter`, the one Jira changed last
/// first.
pub async fn list_issues(state: &AppState, filter: &IssueFilter) -> anyhow::Result<Vec<IssueRow>> {
    Ok(db::issues::list(&state.pool, filter).await?)
}

/// The assignees and the statuses of the issues in the local cache: what the
/// list can be filtered by.
pub async fn filter_options(state: &AppState) -> anyhow::Result<FilterOptions> {
    Ok(db::issues::filter_options(&state.pool).await?)
}

/// One issue from the local cache, with its description and comments as Markdown.
pub async fn get_issue(state: &AppState, key: &IssueKey) -> anyhow::Result<IssueDetail> {
    let (site_url, me) = match &*state.config.read().await {
        Some(config) => (config.site_url.clone(), config.display_name.clone()),
        None => bail!("Sign in first"),
    };
    // A config written before the name was kept, until a sync cycle asks for it
    let me = me.unwrap_or_else(|| "You".to_owned());
    db::issues::get_detail(&state.pool, key, &site_url, &me)
        .await?
        .with_context(|| format!("issue {key} not found"))
}

/// The transitions the issue offers from its current status. They come from
/// the local cache; Jira is asked only for a status of a workflow that was
/// never opened before.
pub async fn get_transitions(state: &AppState, key: &IssueKey) -> anyhow::Result<Vec<Transition>> {
    let (transitions, _refresh) = transitions::get(state, key).await?;
    Ok(transitions)
}

/// Moves the issue to the status `transition` leads to, locally and at once:
/// Jira gets the change from the outbox. `fields` holds the values of the
/// transition's required fields, by field id.
pub async fn transition_issue(
    state: &AppState,
    key: &IssueKey,
    transition: &Transition,
    fields: &serde_json::Value,
) -> anyhow::Result<()> {
    let payload = json!({ "transition": { "id": transition.id }, "fields": fields });
    // One transaction: a crash must not leave a status Jira will never hear
    // of, or an outbox entry for a status the user does not see
    let mut tx = crate::db::begin(&state.pool).await?;
    let found = db::issues::set_pending_status(
        &mut *tx,
        key,
        &transition.to_status,
        transition.to_status_category,
    )
    .await?;
    if !found {
        bail!("issue {key} not found");
    }
    db::sync_queue::enqueue(&mut *tx, "transition", key, &payload.to_string()).await?;
    tx.commit().await?;

    state
        .events
        .send(SyncEvent::IssuesUpdated(vec![key.clone()]))
        .ok();
    state.sync_waker.send(()).ok();
    Ok(())
}

/// The people the issue can be assigned to, in the order Jira lists them. They
/// come from the local cache; Jira is asked only for an issue never opened before.
pub async fn assignable_users(state: &AppState, key: &IssueKey) -> anyhow::Result<Vec<User>> {
    // The answer is filed under the key, so an issue the cache does not have
    // would leave an entry nothing ever uses or removes
    if db::issues::get(&state.pool, key).await?.is_none() {
        bail!("issue {key} not found");
    }
    let (users, _refresh) = assignable::get(state, key).await?;
    Ok(users)
}

/// Assigns the issue to `assignee`, or to nobody, locally and at once: Jira
/// gets the change from the outbox.
pub async fn assign_issue(
    state: &AppState,
    key: &IssueKey,
    assignee: Option<&User>,
) -> anyhow::Result<()> {
    let payload = json!({ "accountId": assignee.map(|user| &user.account_id) });
    // One transaction, as for a transition: the assignee shown and the entry
    // Jira will get must not part ways
    let mut tx = crate::db::begin(&state.pool).await?;
    if !db::issues::set_pending_assignee(&mut *tx, key, assignee).await? {
        bail!("issue {key} not found");
    }
    db::sync_queue::enqueue(&mut *tx, "assignee", key, &payload.to_string()).await?;
    tx.commit().await?;

    state
        .events
        .send(SyncEvent::IssuesUpdated(vec![key.clone()]))
        .ok();
    state.sync_waker.send(()).ok();
    Ok(())
}

/// Queues a plain-text comment on the issue. It shows in the issue as pending
/// at once, and Jira gets it from the outbox. The text is trimmed; a comment
/// without any is refused, and so is a comment on an issue the cache does not have.
pub async fn add_comment(state: &AppState, key: &IssueKey, text: &str) -> anyhow::Result<()> {
    queue_comment(state, key, text, None).await
}

/// `add_comment` for an answer to the comment with the id `comment_id`
/// (`Comment::id`). Jira has one level of replies: an answer to a reply joins
/// the thread of the comment that reply answers.
pub async fn reply_to_comment(
    state: &AppState,
    key: &IssueKey,
    comment_id: &str,
    text: &str,
) -> anyhow::Result<()> {
    queue_comment(state, key, text, Some(comment_id)).await
}

async fn queue_comment(
    state: &AppState,
    key: &IssueKey,
    text: &str,
    reply_to: Option<&str>,
) -> anyhow::Result<()> {
    let text = text.trim();
    if text.is_empty() {
        bail!("a comment cannot be empty");
    }
    // The pending comment is listed with its issue, so without the issue it
    // would be sent and shown nowhere
    if db::issues::get(&state.pool, key).await?.is_none() {
        bail!("issue {key} not found");
    }
    let mut payload = json!({ "text": text });
    if let Some(comment_id) = reply_to {
        payload["parent_id"] = db::issues::top_of_thread(&state.pool, key, comment_id)
            .await?
            .into();
    }
    db::sync_queue::enqueue(&state.pool, "comment", key, &payload.to_string()).await?;

    state
        .events
        .send(SyncEvent::IssuesUpdated(vec![key.clone()]))
        .ok();
    state.sync_waker.send(()).ok();
    Ok(())
}

/// Logs time on the issue, locally and at once: Jira gets the worklog from
/// the outbox, with the `started_at` given here however late it is delivered.
/// `started_at` is Unix seconds. The comment is trimmed and left out when
/// empty. Less than a minute, and an issue the cache does not have, are refused.
pub async fn log_time(
    state: &AppState,
    key: &IssueKey,
    time_spent_secs: u64,
    started_at: i64,
    comment: Option<&str>,
) -> anyhow::Result<()> {
    if time_spent_secs < db::worklogs::MIN_TIME_SPENT_SEC as u64 {
        bail!("the time spent cannot be less than a minute");
    }
    let started_at =
        db::worklogs::started_at_from_unix(started_at).context("the start time is out of range")?;
    let worklog = db::worklogs::NewWorklog {
        issue_key: key.clone(),
        time_spent_sec: i64::try_from(time_spent_secs).context("the time spent is too long")?,
        started_at,
        comment: comment
            .map(str::trim)
            .filter(|comment| !comment.is_empty())
            .map(str::to_owned),
    };
    if db::issues::get(&state.pool, key).await?.is_none() {
        bail!("issue {key} not found");
    }
    // One transaction: the worklog shown and the entry Jira will get must not
    // part ways
    let mut tx = crate::db::begin(&state.pool).await?;
    db::worklogs::log(&mut tx, &worklog).await?;
    tx.commit().await?;

    state
        .events
        .send(SyncEvent::IssuesUpdated(vec![key.clone()]))
        .ok();
    state.sync_waker.send(()).ok();
    Ok(())
}

pub async fn running_timer(state: &AppState) -> anyhow::Result<Option<RunningTimer>> {
    Ok(db::timer::get(&state.pool).await?)
}

/// Replaces a timer that is already running; its time is not logged. An issue
/// the cache does not have is refused.
pub async fn start_timer(state: &AppState, timer: &RunningTimer) -> anyhow::Result<()> {
    if db::issues::get(&state.pool, &timer.issue_key)
        .await?
        .is_none()
    {
        bail!("issue {} not found", timer.issue_key);
    }
    Ok(db::timer::start(&state.pool, timer).await?)
}

/// Logs the time since the timer started as a worklog and stops the timer.
/// `stopped_at` is Unix seconds: the moment of the click, not of this write.
pub async fn stop_timer(state: &AppState, stopped_at: i64) -> anyhow::Result<()> {
    if let Some(key) = db::timer::stop(&state.pool, stopped_at).await? {
        state.events.send(SyncEvent::IssuesUpdated(vec![key])).ok();
        state.sync_waker.send(()).ok();
    }
    Ok(())
}

/// Queues a comment Jira refused again, as it was written. `entry_id` comes
/// from `Comment::rejected`.
pub async fn retry_comment(state: &AppState, entry_id: i64) -> anyhow::Result<()> {
    let key = db::sync_queue::retry_comment(&state.pool, entry_id)
        .await?
        .with_context(|| format!("entry {entry_id} is not a refused comment"))?;
    state.events.send(SyncEvent::IssuesUpdated(vec![key])).ok();
    state.sync_waker.send(()).ok();
    Ok(())
}

/// Throws away a comment Jira refused. `entry_id` comes from `Comment::rejected`.
pub async fn discard_comment(state: &AppState, entry_id: i64) -> anyhow::Result<()> {
    let key = db::sync_queue::discard_comment(&state.pool, entry_id)
        .await?
        .with_context(|| format!("entry {entry_id} is not a refused comment"))?;
    state.events.send(SyncEvent::IssuesUpdated(vec![key])).ok();
    Ok(())
}

/// Queues a worklog Jira or Tempo refused again, as it was logged. `entry_id`
/// comes from `Worklog::rejected`.
pub async fn retry_worklog(state: &AppState, entry_id: i64) -> anyhow::Result<()> {
    let key = db::worklogs::retry(&state.pool, entry_id)
        .await?
        .with_context(|| format!("entry {entry_id} is not a refused worklog"))?;
    state.events.send(SyncEvent::IssuesUpdated(vec![key])).ok();
    state.sync_waker.send(()).ok();
    Ok(())
}

/// Throws away a worklog Jira or Tempo refused. `entry_id` comes from
/// `Worklog::rejected`.
pub async fn discard_worklog(state: &AppState, entry_id: i64) -> anyhow::Result<()> {
    let key = db::worklogs::discard(&state.pool, entry_id)
        .await?
        .with_context(|| format!("entry {entry_id} is not a refused worklog"))?;
    state.events.send(SyncEvent::IssuesUpdated(vec![key])).ok();
    Ok(())
}

/// How many issues `search_issues` returns at most.
pub const SEARCH_LIMIT: usize = 50;

/// The `SEARCH_LIMIT` best matches of `query` among the issues in the local
/// cache that pass `filter`, best first. Every word must match a key, a summary
/// or a description, as a word, a stem or a prefix.
pub async fn search_issues(
    state: &AppState,
    query: &str,
    filter: &IssueFilter,
) -> anyhow::Result<Vec<IssueRow>> {
    Ok(db::issues::search(&state.pool, query, filter, SEARCH_LIMIT as i64).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        models::{Comment, StatusCategory, SyncStatus, Worklog},
        start_without_keychain,
        sync::issue_record,
    };
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{basic_auth, method, path},
    };

    async fn jira(myself: ResponseTemplate) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/myself"))
            .respond_with(myself)
            .mount(&server)
            .await;
        server
    }

    fn myself() -> ResponseTemplate {
        ResponseTemplate::new(200)
            .set_body_json(json!({ "accountId": "5b10a", "displayName": "Ada" }))
    }

    fn nothing_is_saved(data_dir: &std::path::Path, state: &AppState) {
        assert!(!data_dir.join("config.json").exists());
        assert_eq!(state.tokens.load().unwrap(), None);
    }

    #[tokio::test]
    async fn a_fresh_install_has_no_account() {
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();

        assert_eq!(account(&state).await, None);
    }

    #[tokio::test]
    async fn sign_in_saves_the_account_and_keeps_the_token_out_of_the_config() {
        let server = MockServer::start().await;
        // Only the credentials the user typed get an answer
        Mock::given(method("GET"))
            .and(path("/rest/api/3/myself"))
            .and(basic_auth("ada@example.com", "secret-token"))
            .respond_with(myself())
            .mount(&server)
            .await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();

        let typed_url = format!(" {}/ ", server.uri());
        let signed_in = sign_in(&state, &typed_url, "ada@example.com", "secret-token")
            .await
            .unwrap();

        let expected = Account {
            site_url: server.uri(),
            email: "ada@example.com".to_owned(),
            account_id: "5b10a".to_owned(),
        };
        assert_eq!(signed_in, expected);
        assert_eq!(account(&state).await, Some(expected));

        let config = std::fs::read_to_string(data_dir.path().join("config.json")).unwrap();
        let config: serde_json::Value = serde_json::from_str(&config).unwrap();
        assert_eq!(
            config,
            json!({
                "site_url": server.uri(),
                "email": "ada@example.com",
                "account_id": "5b10a",
                "display_name": "Ada",
                "worklog_target": "jira",
                "shortcut_hints": true,
            })
        );
        assert_eq!(
            state.tokens.load().unwrap().as_deref(),
            Some("secret-token")
        );
    }

    #[tokio::test]
    async fn a_restart_uses_the_saved_credentials() {
        let server = jira(myself()).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        let signed_in = sign_in(&state, &server.uri(), "ada@example.com", "secret-token")
            .await
            .unwrap();
        state.pool.close().await;

        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();

        assert_eq!(account(&state).await, Some(signed_in));
        let config = state.config.read().await;
        assert_eq!(config.as_ref().unwrap().api_token, "secret-token");
    }

    #[tokio::test]
    async fn tempo_cannot_be_the_target_without_a_token() {
        let server = jira(myself()).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        sign_in(&state, &server.uri(), "ada@example.com", "secret-token")
            .await
            .unwrap();

        let refused = set_worklog_target(&state, WorklogTarget::Tempo).await;

        assert_eq!(
            refused.unwrap_err().to_string(),
            "Save the Tempo API token first"
        );
        assert_eq!(
            worklog_settings(&state).await.unwrap(),
            WorklogSettings {
                target: WorklogTarget::Jira,
                tempo_token_saved: false
            }
        );
        assert!(save_tempo_token(&state, "  ").await.is_err());
    }

    #[tokio::test]
    async fn the_tempo_token_and_the_target_survive_a_restart_and_the_token_stays_out_of_the_config()
     {
        let server = jira(myself()).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        sign_in(&state, &server.uri(), "ada@example.com", "secret-token")
            .await
            .unwrap();

        save_tempo_token(&state, " tempo-secret ").await.unwrap();
        set_worklog_target(&state, WorklogTarget::Tempo)
            .await
            .unwrap();

        // The token store stands in for the Keychain
        assert_eq!(
            state.tempo_tokens.load().unwrap().as_deref(),
            Some("tempo-secret")
        );
        let config = std::fs::read_to_string(data_dir.path().join("config.json")).unwrap();
        assert!(!config.contains("tempo-secret"));
        state.pool.close().await;

        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        assert_eq!(
            worklog_settings(&state).await.unwrap(),
            WorklogSettings {
                target: WorklogTarget::Tempo,
                tempo_token_saved: true
            }
        );

        sign_out(&state).await.unwrap();
        assert_eq!(state.tempo_tokens.load().unwrap(), None);
    }

    #[tokio::test]
    async fn the_shortcut_hints_are_shown_until_turned_off_and_the_choice_survives_a_restart() {
        let server = jira(myself()).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        // Nobody is signed in, and there is no config to keep the choice
        assert!(shortcut_hints(&state).await);
        assert!(set_shortcut_hints(&state, false).await.is_err());
        sign_in(&state, &server.uri(), "ada@example.com", "secret-token")
            .await
            .unwrap();
        assert!(shortcut_hints(&state).await);

        set_shortcut_hints(&state, false).await.unwrap();
        assert!(!shortcut_hints(&state).await);
        state.pool.close().await;

        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        assert!(!shortcut_hints(&state).await);
        set_shortcut_hints(&state, true).await.unwrap();
        state.pool.close().await;

        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        assert!(shortcut_hints(&state).await);
    }

    fn project_list() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "isLast": true,
            "values": [
                { "id": "10001", "key": "API", "name": "Backend" },
                { "id": "10000", "key": "WEB", "name": "Website" },
            ],
        }))
    }

    #[tokio::test]
    async fn the_watched_projects_are_chosen_from_the_accessible_ones_and_saved() {
        let server = jira(myself()).await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/project/search"))
            .and(basic_auth("ada@example.com", "secret-token"))
            .respond_with(project_list())
            .mount(&server)
            .await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        assert_eq!(setup_step(&state).await.unwrap(), SetupStep::SignIn);
        sign_in(&state, &server.uri(), "ada@example.com", "secret-token")
            .await
            .unwrap();
        assert_eq!(setup_step(&state).await.unwrap(), SetupStep::ChooseProjects);

        let accessible = projects(&state).await.unwrap();
        assert_eq!(
            accessible,
            [
                Project {
                    key: "API".to_owned(),
                    name: "Backend".to_owned()
                },
                Project {
                    key: "WEB".to_owned(),
                    name: "Website".to_owned()
                },
            ]
        );
        assert!(watched_projects(&state).await.unwrap().is_empty());
        watch_projects(&state, &["WEB".to_owned()]).await.unwrap();

        assert_eq!(watched_projects(&state).await.unwrap(), ["WEB"]);
        assert_eq!(setup_step(&state).await.unwrap(), SetupStep::InitialSync);
        state.pool.close().await;
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        assert_eq!(setup_step(&state).await.unwrap(), SetupStep::InitialSync);
        assert_eq!(watched_projects(&state).await.unwrap(), ["WEB"]);
    }

    #[tokio::test]
    async fn watching_no_project_is_refused() {
        let server = jira(myself()).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        sign_in(&state, &server.uri(), "ada@example.com", "secret-token")
            .await
            .unwrap();

        let error = watch_projects(&state, &[]).await.unwrap_err();

        assert_eq!(error.to_string(), "Choose at least one project");
        assert_eq!(setup_step(&state).await.unwrap(), SetupStep::ChooseProjects);
    }

    #[tokio::test]
    async fn a_config_saved_before_project_selection_existed_still_loads() {
        let data_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            data_dir.path().join("config.json"),
            r#"{ "site_url": "https://x.atlassian.net", "email": "ada@example.com", "account_id": "5b10a" }"#,
        )
        .unwrap();
        std::fs::write(data_dir.path().join("api-token"), "secret-token").unwrap();

        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();

        assert_eq!(setup_step(&state).await.unwrap(), SetupStep::ChooseProjects);
    }

    fn paragraph(text: &str) -> serde_json::Value {
        json!({
            "type": "doc",
            "version": 1,
            "content": [{ "type": "paragraph", "content": [{ "type": "text", "text": text }] }],
        })
    }

    /// A signed-in core whose cache holds `issue`, an element of a search page.
    async fn core_with(issue: serde_json::Value) -> (tempfile::TempDir, std::sync::Arc<AppState>) {
        let server = jira(myself()).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        sign_in(&state, &server.uri(), "ada@example.com", "secret-token")
            .await
            .unwrap();
        let record = issue_record::issue_record(&issue).unwrap();
        db::issues::upsert(&state.pool, &record).await.unwrap();
        (data_dir, state)
    }

    #[tokio::test]
    async fn an_issue_is_read_with_its_fields_description_and_comments() {
        let (_data_dir, state) = core_with(json!({
            "id": "10001",
            "key": "WEB-1",
            "fields": {
                "summary": "Fix login",
                "description": paragraph("Login *fails*"),
                "status": { "name": "In Review", "statusCategory": { "key": "indeterminate" } },
                "assignee": { "accountId": "5b10a", "displayName": "Ada Lovelace" },
                "reporter": { "accountId": "7c20b", "displayName": "Grace Hopper" },
                "priority": { "name": "High" },
                "labels": ["auth", "regression"],
                "comment": {
                    "total": 3,
                    "comments": [
                        {
                            "author": { "accountId": "7c20b", "displayName": "Grace Hopper" },
                            "created": "2026-03-28T09:15:00.000+0100",
                            "body": paragraph("Seen on staging"),
                        },
                        {
                            "author": { "accountId": "5b10a", "displayName": "Ada Lovelace" },
                            "created": "2026-03-29T01:30:00.000+0100",
                            "body": paragraph("Fixed"),
                        },
                    ],
                },
                "project": { "key": "WEB" },
                "issuetype": { "name": "Bug" },
                "updated": "2026-03-29T01:30:00.000+0100",
            },
        }))
        .await;
        let site_url = account(&state).await.unwrap().site_url;

        let issue = get_issue(&state, &"WEB-1".into()).await.unwrap();

        let comment = |author: &str, created: &str, body: &str| Comment {
            id: None,
            reply_to: None,
            author_name: author.to_owned(),
            created: created.to_owned(),
            body_md: body.to_owned(),
            pending: false,
            rejected: None,
        };
        assert_eq!(
            issue,
            IssueDetail {
                key: "WEB-1".into(),
                summary: "Fix login".to_owned(),
                status: "In Review".to_owned(),
                status_category: StatusCategory::InProgress,
                sync_status: SyncStatus::Synced,
                status_pending: false,
                issue_type: Some("Bug".to_owned()),
                assignee_name: Some("Ada Lovelace".to_owned()),
                reporter_name: Some("Grace Hopper".to_owned()),
                priority: Some("High".to_owned()),
                labels: vec!["auth".to_owned(), "regression".to_owned()],
                description_md: "Login \\*fails\\*".to_owned(),
                comments: vec![
                    comment(
                        "Grace Hopper",
                        "2026-03-28T09:15:00.000+0100",
                        "Seen on staging"
                    ),
                    comment("Ada Lovelace", "2026-03-29T01:30:00.000+0100", "Fixed"),
                ],
                comment_total: 3,
                worklogs: vec![],
                url: format!("{site_url}/browse/WEB-1"),
            }
        );
    }

    #[tokio::test]
    async fn the_worklogs_of_an_issue_are_listed_newest_first_with_their_authors_and_state() {
        let (_data_dir, state) = core_with(json!({
            "id": "10001",
            "key": "WEB-1",
            "fields": {
                "summary": "Fix login",
                "status": { "name": "To Do", "statusCategory": { "key": "new" } },
                "project": { "key": "WEB" },
                "updated": "2026-03-29T01:30:00.000+0100",
            },
        }))
        .await;
        let grace = db::worklogs::RemoteWorklog {
            remote_id: "1".to_owned(),
            issue_id: "10001".to_owned(),
            author_name: "Grace Hopper".to_owned(),
            time_spent_sec: 5580,
            started_at: "2026-03-27T09:00:00.000Z".to_owned(),
            comment: Some("Pairing".to_owned()),
            tempo_id: None,
        };
        db::worklogs::upsert_remote(&mut state.pool.acquire().await.unwrap(), &grace)
            .await
            .unwrap();
        log_time(&state, &"WEB-1".into(), 900, 1_774_771_200, Some("Review"))
            .await
            .unwrap();
        log_time(&state, &"WEB-1".into(), 1800, 1_774_857_600, None)
            .await
            .unwrap();
        sqlx::query("UPDATE worklogs SET sync_status = 'failed' WHERE time_spent_sec = 1800")
            .execute(&state.pool)
            .await
            .unwrap();

        let worklogs = get_issue(&state, &"WEB-1".into()).await.unwrap().worklogs;

        let worklog =
            |author: &str, started_at, time_spent_secs, comment: Option<&str>, status| Worklog {
                author_name: author.to_owned(),
                started_at,
                time_spent_secs,
                comment: comment.map(str::to_owned),
                status,
                rejected: None,
            };
        assert_eq!(
            worklogs,
            [
                worklog("Ada", 1_774_857_600, 1800, None, SyncStatus::Failed),
                worklog(
                    "Ada",
                    1_774_771_200,
                    900,
                    Some("Review"),
                    SyncStatus::Pending
                ),
                worklog(
                    "Grace Hopper",
                    1_774_602_000,
                    5580,
                    Some("Pairing"),
                    SyncStatus::Synced
                ),
            ]
        );
    }

    #[tokio::test]
    async fn stopping_the_timer_logs_the_time_up_to_the_click() {
        let (_data_dir, state) = core_with(json!({
            "id": "10001",
            "key": "WEB-1",
            "fields": {
                "summary": "Fix login",
                "status": { "name": "To Do", "statusCategory": { "key": "new" } },
                "project": { "key": "WEB" },
                "updated": "2026-03-29T01:30:00.000+0100",
            },
        }))
        .await;
        let timer = RunningTimer {
            issue_key: "WEB-1".into(),
            started_at: 1_774_771_200,
        };
        start_timer(&state, &timer).await.unwrap();
        assert_eq!(running_timer(&state).await.unwrap(), Some(timer));

        stop_timer(&state, 1_774_771_200 + 3_725).await.unwrap();

        assert_eq!(running_timer(&state).await.unwrap(), None);
        let worklogs = get_issue(&state, &"WEB-1".into()).await.unwrap().worklogs;
        assert_eq!(worklogs.len(), 1);
        assert_eq!(worklogs[0].time_spent_secs, 3_725);
        assert_eq!(worklogs[0].started_at, 1_774_771_200);
        assert_eq!(worklogs[0].status, SyncStatus::Pending);
    }

    #[tokio::test]
    async fn a_timer_cannot_be_started_on_an_issue_the_cache_does_not_have() {
        let (_data_dir, state) = core_with(json!({
            "id": "10001",
            "key": "WEB-1",
            "fields": {
                "summary": "Fix login",
                "status": { "name": "To Do", "statusCategory": { "key": "new" } },
                "project": { "key": "WEB" },
                "updated": "2026-03-29T01:30:00.000+0100",
            },
        }))
        .await;
        let timer = RunningTimer {
            issue_key: "WEB-9".into(),
            started_at: 1_774_771_200,
        };

        assert!(start_timer(&state, &timer).await.is_err());
        assert_eq!(running_timer(&state).await.unwrap(), None);
    }

    #[tokio::test]
    async fn an_issue_without_the_optional_fields_is_read_with_them_empty() {
        // What a cache filled before the detail view existed holds, too
        let (_data_dir, state) = core_with(json!({
            "id": "10001",
            "key": "WEB-1",
            "fields": {
                "summary": "Fix login",
                "description": null,
                "status": { "name": "To Do", "statusCategory": { "key": "new" } },
                "assignee": null,
                "project": { "key": "WEB" },
                "updated": "2026-03-29T01:30:00.000+0100",
            },
        }))
        .await;

        let issue = get_issue(&state, &"WEB-1".into()).await.unwrap();

        assert_eq!(issue.issue_type, None);
        assert_eq!(issue.assignee_name, None);
        assert_eq!(issue.reporter_name, None);
        assert_eq!(issue.priority, None);
        assert_eq!(issue.labels, [] as [&str; 0]);
        assert_eq!(issue.description_md, "");
        assert_eq!(issue.comments, []);
        assert_eq!(issue.comment_total, 0);

        let error = get_issue(&state, &"WEB-2".into()).await.unwrap_err();
        assert_eq!(error.to_string(), "issue WEB-2 not found");
    }

    fn start() -> Transition {
        Transition {
            id: "21".to_owned(),
            name: "Start".to_owned(),
            to_status: "In Progress".to_owned(),
            to_status_category: StatusCategory::InProgress,
            required_fields: Vec::new(),
        }
    }

    fn todo_issue() -> serde_json::Value {
        json!({
            "id": "10001",
            "key": "WEB-1",
            "fields": {
                "summary": "Fix login",
                "status": { "name": "To Do", "statusCategory": { "key": "new" } },
                "project": { "key": "WEB" },
                "updated": "2026-03-29T01:30:00.000+0100",
            },
        })
    }

    /// The type and the payload of every outbox entry that waits for Jira.
    async fn queued(state: &AppState) -> Vec<(String, String)> {
        sqlx::query_as(
            "SELECT entity_type, payload FROM sync_queue WHERE status = 'pending' ORDER BY id",
        )
        .fetch_all(&state.pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_transition_changes_the_status_at_once_and_queues_it_for_jira() {
        let (_data_dir, state) = core_with(todo_issue()).await;
        let fields = json!({ "resolution": { "id": "10000" } });

        transition_issue(&state, &"WEB-1".into(), &start(), &fields)
            .await
            .unwrap();

        let issue = get_issue(&state, &"WEB-1".into()).await.unwrap();
        assert_eq!(issue.status, "In Progress");
        assert_eq!(issue.status_category, StatusCategory::InProgress);
        assert_eq!(issue.sync_status, SyncStatus::Pending);
        assert!(issue.status_pending);
        let queued = queued(&state).await;
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].0, "transition");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&queued[0].1).unwrap(),
            json!({ "transition": { "id": "21" }, "fields": fields })
        );
    }

    #[tokio::test]
    async fn a_transition_that_cannot_be_queued_leaves_the_issue_as_it_was() {
        let (_data_dir, state) = core_with(todo_issue()).await;
        sqlx::query(
            "CREATE TRIGGER outbox_is_broken BEFORE INSERT ON sync_queue
             BEGIN SELECT RAISE(ABORT, 'the outbox is broken'); END",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        let error = transition_issue(&state, &"WEB-1".into(), &start(), &json!({}))
            .await
            .unwrap_err();

        assert!(
            error.to_string().contains("the outbox is broken"),
            "{error}"
        );
        let issue = get_issue(&state, &"WEB-1".into()).await.unwrap();
        assert_eq!(issue.status, "To Do");
        assert_eq!(issue.sync_status, SyncStatus::Synced);
    }

    #[tokio::test]
    async fn a_transition_of_an_issue_the_cache_does_not_have_queues_nothing() {
        let (_data_dir, state) = core_with(todo_issue()).await;

        let error = transition_issue(&state, &"WEB-2".into(), &start(), &json!({}))
            .await
            .unwrap_err();

        assert_eq!(error.to_string(), "issue WEB-2 not found");
        assert_eq!(queued(&state).await, []);
    }

    fn grace() -> User {
        User {
            account_id: "7c20b".to_owned(),
            display_name: "Grace Hopper".to_owned(),
        }
    }

    fn assigned_issue() -> serde_json::Value {
        let mut issue = todo_issue();
        issue["fields"]["assignee"] =
            json!({ "accountId": "5b10a", "displayName": "Ada Lovelace" });
        issue
    }

    #[tokio::test]
    async fn an_assignee_shows_at_once_and_is_queued_for_jira() {
        let (_data_dir, state) = core_with(assigned_issue()).await;

        assign_issue(&state, &"WEB-1".into(), Some(&grace()))
            .await
            .unwrap();

        let issue = get_issue(&state, &"WEB-1".into()).await.unwrap();
        assert_eq!(issue.assignee_name.as_deref(), Some("Grace Hopper"));
        assert_eq!(issue.sync_status, SyncStatus::Pending);
        // The status is the one Jira has
        assert!(!issue.status_pending);
        let row = db::issues::get(&state.pool, &"WEB-1".into()).await.unwrap();
        assert_eq!(row.unwrap().assignee_name.as_deref(), Some("Grace Hopper"));
        let queued = queued(&state).await;
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].0, "assignee");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&queued[0].1).unwrap(),
            json!({ "accountId": "7c20b" })
        );
    }

    #[tokio::test]
    async fn unassigning_clears_the_assignee_at_once_and_queues_a_null() {
        let (_data_dir, state) = core_with(assigned_issue()).await;

        assign_issue(&state, &"WEB-1".into(), None).await.unwrap();

        let issue = get_issue(&state, &"WEB-1".into()).await.unwrap();
        assert_eq!(issue.assignee_name, None);
        assert_eq!(issue.sync_status, SyncStatus::Pending);
        let queued = queued(&state).await;
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].0, "assignee");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&queued[0].1).unwrap(),
            json!({ "accountId": null })
        );
    }

    #[tokio::test]
    async fn an_assignee_that_cannot_be_queued_leaves_the_issue_as_it_was() {
        let (_data_dir, state) = core_with(assigned_issue()).await;
        sqlx::query(
            "CREATE TRIGGER outbox_is_broken BEFORE INSERT ON sync_queue
             BEGIN SELECT RAISE(ABORT, 'the outbox is broken'); END",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        let error = assign_issue(&state, &"WEB-1".into(), Some(&grace()))
            .await
            .unwrap_err();

        assert!(
            error.to_string().contains("the outbox is broken"),
            "{error}"
        );
        let issue = get_issue(&state, &"WEB-1".into()).await.unwrap();
        assert_eq!(issue.assignee_name.as_deref(), Some("Ada Lovelace"));
        assert_eq!(issue.sync_status, SyncStatus::Synced);
    }

    #[tokio::test]
    async fn an_assignee_of_an_issue_the_cache_does_not_have_queues_nothing() {
        let (_data_dir, state) = core_with(todo_issue()).await;

        let error = assign_issue(&state, &"WEB-2".into(), Some(&grace()))
            .await
            .unwrap_err();

        assert_eq!(error.to_string(), "issue WEB-2 not found");
        assert_eq!(queued(&state).await, []);
    }

    #[tokio::test]
    async fn a_comment_shows_as_pending_at_once_and_is_queued_for_jira() {
        let (_data_dir, state) = core_with(todo_issue()).await;

        add_comment(&state, &"WEB-1".into(), "  Seen on staging\nand *prod*\n ")
            .await
            .unwrap();

        let issue = get_issue(&state, &"WEB-1".into()).await.unwrap();
        assert_eq!(
            issue.comments,
            [Comment {
                id: None,
                reply_to: None,
                author_name: "Ada".to_owned(),
                created: String::new(),
                body_md: "Seen on staging\\\nand \\*prod\\*".to_owned(),
                pending: true,
                rejected: None,
            }]
        );
        assert_eq!(issue.comment_total, 1);
        // The issue itself has no change of its own to wait for
        assert_eq!(issue.sync_status, SyncStatus::Synced);
        let queued = queued(&state).await;
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].0, "comment");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&queued[0].1).unwrap(),
            json!({ "text": "Seen on staging\nand *prod*" })
        );
    }

    /// WEB-1 with a comment of Grace, one of Ada, an answer of Ada to Grace
    /// and an answer to a comment the issue does not have.
    fn discussed_issue() -> serde_json::Value {
        let comment = |id: &str, author: &str, text: &str| {
            json!({
                "id": id,
                "author": { "displayName": author },
                "created": "2026-03-28T09:15:00.000+0100",
                "body": paragraph(text),
            })
        };
        let mut answer = comment("10102", "Ada Lovelace", "Which build?");
        answer["parentId"] = json!("10100");
        let mut orphan = comment("10103", "Grace Hopper", "Agreed");
        orphan["parentId"] = json!("10050");
        let mut issue = todo_issue();
        issue["fields"]["comment"] = json!({ "total": 4, "comments": [
            comment("10100", "Grace Hopper", "Seen on staging"),
            comment("10101", "Ada Lovelace", "Cannot reproduce"),
            answer,
            orphan,
        ] });
        issue
    }

    /// The comments of WEB-1: the text of each and the author it answers.
    async fn thread(state: &AppState) -> Vec<(String, Option<Option<String>>)> {
        let issue = get_issue(state, &"WEB-1".into()).await.unwrap();
        issue
            .comments
            .into_iter()
            .map(|comment| {
                let answers = comment.reply_to.map(|reply_to| reply_to.author_name);
                (comment.body_md, answers)
            })
            .collect()
    }

    #[tokio::test]
    async fn a_reply_is_listed_after_the_comment_it_answers_with_the_author_of_that_one() {
        let (_data_dir, state) = core_with(discussed_issue()).await;

        let grace = Some(Some("Grace Hopper".to_owned()));
        assert_eq!(
            thread(&state).await,
            [
                ("Seen on staging".to_owned(), None),
                ("Which build?".to_owned(), grace),
                ("Cannot reproduce".to_owned(), None),
                // Its parent is not among the comments
                ("Agreed".to_owned(), Some(None)),
            ]
        );
        let issue = get_issue(&state, &"WEB-1".into()).await.unwrap();
        assert_eq!(issue.comments[0].id.as_deref(), Some("10100"));
    }

    #[tokio::test]
    async fn a_reply_is_queued_with_its_parent_and_shows_as_pending_in_the_thread() {
        let (_data_dir, state) = core_with(discussed_issue()).await;

        reply_to_comment(&state, &"WEB-1".into(), "10101", " Try build 42 ")
            .await
            .unwrap();
        // An answer to a reply: Jira takes it as one more reply to Grace's comment
        reply_to_comment(&state, &"WEB-1".into(), "10102", "Build 41")
            .await
            .unwrap();

        let payloads: Vec<serde_json::Value> = queued(&state)
            .await
            .iter()
            .map(|(_, payload)| serde_json::from_str(payload).unwrap())
            .collect();
        assert_eq!(
            payloads,
            [
                json!({ "text": "Try build 42", "parent_id": "10101" }),
                json!({ "text": "Build 41", "parent_id": "10100" }),
            ]
        );
        let grace = Some(Some("Grace Hopper".to_owned()));
        let ada = Some(Some("Ada Lovelace".to_owned()));
        assert_eq!(
            thread(&state).await,
            [
                ("Seen on staging".to_owned(), None),
                ("Which build?".to_owned(), grace.clone()),
                ("Build 41".to_owned(), grace),
                ("Cannot reproduce".to_owned(), None),
                ("Try build 42".to_owned(), ada),
                ("Agreed".to_owned(), Some(None)),
            ]
        );
        let issue = get_issue(&state, &"WEB-1".into()).await.unwrap();
        assert!(issue.comments[2].pending);
        assert_eq!(issue.comment_total, 6);
    }

    #[tokio::test]
    async fn an_empty_reply_is_refused_and_queues_nothing() {
        let (_data_dir, state) = core_with(discussed_issue()).await;

        let error = reply_to_comment(&state, &"WEB-1".into(), "10100", " \n")
            .await
            .unwrap_err();

        assert_eq!(error.to_string(), "a comment cannot be empty");
        assert_eq!(queued(&state).await, []);
    }

    #[tokio::test]
    async fn a_comment_on_an_issue_the_cache_does_not_have_queues_nothing() {
        let (_data_dir, state) = core_with(todo_issue()).await;

        let error = add_comment(&state, &"WEB-2".into(), "Seen on staging")
            .await
            .unwrap_err();

        assert_eq!(error.to_string(), "issue WEB-2 not found");
        assert_eq!(queued(&state).await, []);
    }

    #[tokio::test]
    async fn an_empty_comment_is_refused_and_queues_nothing() {
        let (_data_dir, state) = core_with(todo_issue()).await;

        for text in ["", " \n\t "] {
            let error = add_comment(&state, &"WEB-1".into(), text)
                .await
                .unwrap_err();
            assert_eq!(error.to_string(), "a comment cannot be empty");
        }

        assert_eq!(queued(&state).await, []);
    }

    #[tokio::test]
    async fn signing_out_forgets_the_account_and_a_restart_asks_to_sign_in() {
        let (data_dir, state) = core_with(todo_issue()).await;
        let fields = json!({});
        transition_issue(&state, &"WEB-1".into(), &start(), &fields)
            .await
            .unwrap();
        let position = db::transitions::Position {
            project_key: "WEB".to_owned(),
            issue_type: "Task".to_owned(),
            status: "To Do".to_owned(),
        };
        db::transitions::put(&state.pool, &position, &[start()])
            .await
            .unwrap();
        assert_eq!(unsent_changes(&state).await.unwrap(), 1);

        sign_out(&state).await.unwrap();

        nothing_is_saved(data_dir.path(), &state);
        assert_eq!(account(&state).await, None);
        assert_eq!(setup_step(&state).await.unwrap(), SetupStep::SignIn);
        assert!(
            list_issues(&state, &IssueFilter::default())
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            search_issues(&state, "login", &IssueFilter::default())
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(unsent_changes(&state).await.unwrap(), 0);
        let cached = db::transitions::get(&state.pool, &position).await.unwrap();
        assert!(cached.is_none());
        state.pool.close().await;

        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        assert_eq!(setup_step(&state).await.unwrap(), SetupStep::SignIn);
    }

    #[tokio::test]
    async fn the_same_account_signs_in_again_without_a_restart() {
        let server = jira(myself()).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        let first = sign_in(&state, &server.uri(), "ada@example.com", "secret-token")
            .await
            .unwrap();
        sign_out(&state).await.unwrap();

        let again = sign_in(&state, &server.uri(), "ada@example.com", "secret-token")
            .await
            .unwrap();

        assert_eq!(again, first);
        assert_eq!(account(&state).await, Some(first));
        assert_eq!(setup_step(&state).await.unwrap(), SetupStep::ChooseProjects);
    }

    #[tokio::test]
    async fn another_account_does_not_get_the_cache_of_the_previous_one() {
        let (_data_dir, state) = core_with(todo_issue()).await;
        sign_out(&state).await.unwrap();
        let other = ResponseTemplate::new(200)
            .set_body_json(json!({ "accountId": "7c20b", "displayName": "Grace" }));
        let server = jira(other).await;

        let signed_in = sign_in(&state, &server.uri(), "grace@example.com", "her-token")
            .await
            .unwrap();

        assert_eq!(signed_in.account_id, "7c20b");
        assert_eq!(setup_step(&state).await.unwrap(), SetupStep::ChooseProjects);
        assert!(
            list_issues(&state, &IssueFilter::default())
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            search_issues(&state, "login", &IssueFilter::default())
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(state.tokens.load().unwrap().as_deref(), Some("her-token"));
    }

    #[tokio::test]
    async fn another_account_starts_from_an_empty_cache_when_the_config_was_lost() {
        // The user deleted the config by hand, as they had to before sign-out existed
        let (data_dir, state) = core_with(todo_issue()).await;
        std::fs::remove_file(data_dir.path().join("config.json")).unwrap();
        state.pool.close().await;
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        assert_eq!(
            list_issues(&state, &IssueFilter::default())
                .await
                .unwrap()
                .len(),
            1
        );
        let server = jira(myself_grace()).await;

        sign_in(&state, &server.uri(), "grace@example.com", "her-token")
            .await
            .unwrap();

        assert!(
            list_issues(&state, &IssueFilter::default())
                .await
                .unwrap()
                .is_empty()
        );
    }

    fn myself_grace() -> ResponseTemplate {
        ResponseTemplate::new(200)
            .set_body_json(json!({ "accountId": "7c20b", "displayName": "Grace" }))
    }

    /// A signed-in core with one status change that Jira has not received and
    /// a running timer, and the Jira it is signed in to.
    async fn core_with_an_unsent_change()
    -> (MockServer, tempfile::TempDir, std::sync::Arc<AppState>) {
        let server = jira(myself()).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        sign_in(&state, &server.uri(), "ada@example.com", "secret-token")
            .await
            .unwrap();
        let record = issue_record::issue_record(&todo_issue()).unwrap();
        db::issues::upsert(&state.pool, &record).await.unwrap();
        transition_issue(&state, &"WEB-1".into(), &start(), &json!({}))
            .await
            .unwrap();
        start_timer(&state, &timer_on_web_1()).await.unwrap();
        (server, data_dir, state)
    }

    fn timer_on_web_1() -> RunningTimer {
        RunningTimer {
            issue_key: "WEB-1".into(),
            started_at: 1_700_000_000,
        }
    }

    /// The issue with its unsent status, the outbox entry and the timer.
    async fn nothing_is_lost(state: &AppState) {
        let issues = list_issues(state, &IssueFilter::default()).await.unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].status, "In Progress");
        assert_eq!(unsent_changes(state).await.unwrap(), 1);
        assert_eq!(running_timer(state).await.unwrap(), Some(timer_on_web_1()));
    }

    /// Runs the outbox against `server`, which must get the status change.
    async fn the_unsent_change_is_delivered(state: &AppState, server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/rest/api/3/issue/WEB-1/transitions"))
            .and(basic_auth("ada@example.com", "secret-token"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(server)
            .await;
        let config = state.config.read().await.clone().unwrap();
        crate::sync::outbound::process_queue(state, &config, &mut Vec::new())
            .await
            .unwrap();
        assert_eq!(unsent_changes(state).await.unwrap(), 0);
        server.verify().await;
    }

    /// The restart finds nobody signed in and nothing lost; the sign-in of
    /// the same account that follows keeps it all and the change is delivered.
    async fn the_next_sign_in_of_the_account_keeps_everything(
        server: &MockServer,
        data_dir: &std::path::Path,
    ) {
        let (state, _events) = start_without_keychain(data_dir).await.unwrap();
        assert_eq!(setup_step(&state).await.unwrap(), SetupStep::SignIn);
        nothing_is_lost(&state).await;

        sign_in(&state, &server.uri(), "ada@example.com", "secret-token")
            .await
            .unwrap();

        assert_eq!(setup_step(&state).await.unwrap(), SetupStep::ChooseProjects);
        nothing_is_lost(&state).await;
        the_unsent_change_is_delivered(&state, server).await;
        state.pool.close().await;
    }

    #[tokio::test]
    async fn a_config_file_cut_short_keeps_the_unsent_changes_for_the_next_sign_in_of_the_account()
    {
        let config_file = |data_dir: &tempfile::TempDir| data_dir.path().join("config.json");

        // What a build that truncated the file before writing it could leave
        let (server, data_dir, state) = core_with_an_unsent_change().await;
        state.pool.close().await;
        std::fs::write(config_file(&data_dir), "").unwrap();
        the_next_sign_in_of_the_account_keeps_everything(&server, data_dir.path()).await;

        let (server, data_dir, state) = core_with_an_unsent_change().await;
        state.pool.close().await;
        let written = std::fs::read(config_file(&data_dir)).unwrap();
        std::fs::write(config_file(&data_dir), &written[..written.len() / 2]).unwrap();
        the_next_sign_in_of_the_account_keeps_everything(&server, data_dir.path()).await;
    }

    #[tokio::test]
    async fn a_database_of_a_build_that_did_not_record_its_owner_gets_it_at_the_next_start() {
        let (server, data_dir, state) = core_with_an_unsent_change().await;
        sqlx::raw_sql("DELETE FROM cache_owner")
            .execute(&state.pool)
            .await
            .unwrap();
        state.pool.close().await;
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        state.pool.close().await;

        std::fs::write(data_dir.path().join("config.json"), "").unwrap();
        the_next_sign_in_of_the_account_keeps_everything(&server, data_dir.path()).await;
    }

    #[tokio::test]
    async fn a_token_the_store_does_not_give_is_asked_for_and_nothing_else_is_lost() {
        let (server, data_dir, state) = core_with_an_unsent_change().await;
        state.pool.close().await;
        // The config is intact. A Keychain that refuses access reads the same
        // to `auth::load` as one without the item.
        state.tokens.delete().unwrap();

        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        assert_eq!(setup_step(&state).await.unwrap(), SetupStep::EnterToken);
        assert_eq!(
            account(&state).await.map(|account| account.email),
            Some("ada@example.com".to_owned())
        );
        nothing_is_lost(&state).await;

        renew_token(&state, "secret-token").await.unwrap();

        assert_eq!(setup_step(&state).await.unwrap(), SetupStep::ChooseProjects);
        assert_eq!(
            state.tokens.load().unwrap().as_deref(),
            Some("secret-token")
        );
        nothing_is_lost(&state).await;
        the_unsent_change_is_delivered(&state, &server).await;
    }

    #[tokio::test]
    async fn another_account_is_refused_while_changes_are_unsent_and_deletes_them_only_when_told_to()
     {
        let (ada, data_dir, state) = core_with_an_unsent_change().await;
        state.pool.close().await;
        std::fs::write(data_dir.path().join("config.json"), "").unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        let server = jira(myself_grace()).await;

        let error = sign_in(&state, &server.uri(), "grace@example.com", "her-token")
            .await
            .unwrap_err();

        let unsent: &UnsentChanges = error.downcast_ref().expect("the reason is typed");
        assert_eq!(unsent.count, 1);
        assert_eq!(
            unsent.owner,
            Some(Account {
                site_url: ada.uri(),
                email: "ada@example.com".to_owned(),
                account_id: "5b10a".to_owned(),
            })
        );
        assert_eq!(setup_step(&state).await.unwrap(), SetupStep::SignIn);
        assert_eq!(
            state.tokens.load().unwrap().as_deref(),
            Some("secret-token")
        );
        nothing_is_lost(&state).await;

        let signed_in = sign_in_discarding(&state, &server.uri(), "grace@example.com", "her-token")
            .await
            .unwrap();

        assert_eq!(signed_in.account_id, "7c20b");
        assert!(
            list_issues(&state, &IssueFilter::default())
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(unsent_changes(&state).await.unwrap(), 0);
        assert_eq!(running_timer(&state).await.unwrap(), None);
        assert_eq!(db::owner(&state.pool).await.unwrap(), Some(signed_in));
    }

    #[tokio::test]
    async fn a_change_of_the_watched_projects_that_fails_leaves_the_list_and_the_cache_agreeing() {
        let (data_dir, state) = core_with(todo_issue()).await;
        watch_projects(&state, &["WEB".to_owned()]).await.unwrap();
        // The list is written first, and then the delete of WEB-1 fails
        sqlx::raw_sql(
            "CREATE TRIGGER disk_full BEFORE DELETE ON issues
             BEGIN SELECT RAISE(ABORT, 'disk full'); END",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        watch_projects(&state, &["OPS".to_owned()])
            .await
            .unwrap_err();

        assert_eq!(watched_projects(&state).await.unwrap(), ["WEB"]);
        state.pool.close().await;
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        assert_eq!(watched_projects(&state).await.unwrap(), ["WEB"]);
        let listed = list_issues(&state, &IssueFilter::default()).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].key, "WEB-1".into());
    }

    /// Closes the core and rewrites its `config.json` the way a build that
    /// kept the sync state there wrote it.
    async fn written_by_an_earlier_build(data_dir: &std::path::Path, state: &AppState) {
        state.pool.close().await;
        let config_file = data_dir.join("config.json");
        let mut config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&config_file).unwrap()).unwrap();
        config["watched_projects"] = json!(["WEB", "API", "OPS"]);
        config["pending_projects"] = json!(["OPS"]);
        config["last_sync_at"] = json!(4_000);
        config["last_reconcile_at"] = json!(3_000);
        config["last_worklog_sync_at"] = json!(2_000);
        config["last_worklog_delete_sync_at"] = json!(1_000);
        std::fs::write(&config_file, config.to_string()).unwrap();
    }

    #[tokio::test]
    async fn the_sync_state_an_earlier_build_kept_in_the_config_moves_to_the_database() {
        let (data_dir, state) = core_with(todo_issue()).await;
        written_by_an_earlier_build(data_dir.path(), &state).await;

        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();

        let moved = db::sync_state::SyncState {
            watched_projects: ["WEB", "API", "OPS"].map(str::to_owned).into(),
            pending_projects: vec!["OPS".to_owned()],
            last_sync_at: Some(4_000),
            last_reconcile_at: 3_000,
            last_worklog_sync_at: 2_000,
            last_worklog_delete_sync_at: 1_000,
        };
        assert_eq!(db::sync_state::load(&state.pool).await.unwrap(), moved);
        // Only OPS is left to download, as before the update
        assert_eq!(setup_step(&state).await.unwrap(), SetupStep::InitialSync);
        let config = std::fs::read(data_dir.path().join("config.json")).unwrap();
        let config: serde_json::Value = serde_json::from_slice(&config).unwrap();
        assert_eq!(config["account_id"], "5b10a");
        assert!(config.get("watched_projects").is_none());
        assert!(config.get("last_sync_at").is_none());

        // The database has moved on: what the config said is not read again
        watch_projects(&state, &["WEB".to_owned()]).await.unwrap();
        written_by_an_earlier_build(data_dir.path(), &state).await;
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        assert_eq!(watched_projects(&state).await.unwrap(), ["WEB"]);
        assert_eq!(setup_step(&state).await.unwrap(), SetupStep::Ready);
    }

    /// A data dir with the account of `from` and no database.
    fn without_its_database(from: &std::path::Path) -> tempfile::TempDir {
        let data_dir = tempfile::tempdir().unwrap();
        for file in ["config.json", "api-token"] {
            std::fs::copy(from.join(file), data_dir.path().join(file)).unwrap();
        }
        data_dir
    }

    #[tokio::test]
    async fn a_database_that_went_missing_beside_a_saved_account_gets_the_initial_sync() {
        let (with_database, state) = core_with(todo_issue()).await;
        written_by_an_earlier_build(with_database.path(), &state).await;

        // The config of an earlier build says the sync has finished, of a
        // database that is gone
        let data_dir = without_its_database(with_database.path());
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        assert!(account(&state).await.is_some());
        assert_eq!(setup_step(&state).await.unwrap(), SetupStep::InitialSync);
        assert_eq!(last_sync_at(&state).await.unwrap(), None);
        assert_eq!(
            watched_projects(&state).await.unwrap(),
            ["WEB", "API", "OPS"]
        );

        // The projects were in the database too: they are chosen again
        let data_dir = without_its_database(data_dir.path());
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        assert!(account(&state).await.is_some());
        assert_eq!(setup_step(&state).await.unwrap(), SetupStep::ChooseProjects);
    }

    #[tokio::test]
    async fn rejected_credentials_are_explained_and_not_saved() {
        let server = jira(ResponseTemplate::new(401)).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();

        let error = sign_in(&state, &server.uri(), "ada@example.com", "wrong")
            .await
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "Jira rejected the email or the API token"
        );
        assert_eq!(account(&state).await, None);
        nothing_is_saved(data_dir.path(), &state);
    }

    #[tokio::test]
    async fn a_wrong_site_is_explained_and_not_saved() {
        let not_jira = jira(ResponseTemplate::new(404)).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();

        let error = sign_in(&state, &not_jira.uri(), "ada@example.com", "token")
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "{} answered 404 Not Found. Check the site URL",
                not_jira.uri()
            )
        );

        // Nothing listens on port 1
        let error = sign_in(&state, "http://127.0.0.1:1", "ada@example.com", "token")
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("Could not reach http://127.0.0.1:1: "),
            "{error}"
        );

        nothing_is_saved(data_dir.path(), &state);
    }

    #[tokio::test]
    async fn an_empty_field_is_refused_without_a_request() {
        let server = jira(myself()).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();

        let error = sign_in(&state, &server.uri(), "ada@example.com", "  ")
            .await
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "Enter the site URL, the email and the API token"
        );
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_site_typed_without_a_scheme_gets_https() {
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();

        // Port 1 refuses the connection; the error names the URL that was tried
        let error = sign_in(&state, "127.0.0.1:1/", "ada@example.com", "token")
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .starts_with("Could not reach https://127.0.0.1:1: "),
            "{error}"
        );
    }
}
