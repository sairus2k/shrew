//! The Tempo Timesheets REST API, used for creating worklogs only.

use reqwest::StatusCode;
use serde_json::Value;
use std::fmt;

#[derive(Debug)]
pub(crate) enum TempoError {
    /// The Tempo token is wrong or has expired (401).
    Unauthorized,
    Status(StatusCode, String),
    Http(reqwest::Error),
}

impl fmt::Display for TempoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthorized => f.write_str("Tempo rejected the API token"),
            Self::Status(status, body) => write!(f, "Tempo returned {status}: {body}"),
            Self::Http(error) => write!(f, "Tempo request failed: {error}"),
        }
    }
}

impl std::error::Error for TempoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Http(error) => Some(error),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for TempoError {
    fn from(error: reqwest::Error) -> Self {
        Self::Http(error)
    }
}

pub(crate) struct TempoClient {
    http: reqwest::Client,
    base_url: String,
    api_token: String,
}

impl TempoClient {
    /// `base_url` is the API root, such as `https://api.tempo.io/4`.
    pub(crate) fn new(base_url: &str, api_token: &str) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_token: api_token.to_owned(),
        }
    }

    /// Creates the worklog and returns the id of the Jira worklog Tempo
    /// mirrored it into.
    pub(crate) async fn create_worklog(&self, body: &Value) -> Result<String, TempoError> {
        let response = self
            .http
            .post(format!("{}/worklogs", self.base_url))
            .bearer_auth(&self.api_token)
            .json(body)
            .send()
            .await?;
        match response.status() {
            StatusCode::UNAUTHORIZED => Err(TempoError::Unauthorized),
            status if !status.is_success() => {
                let body = response.text().await.unwrap_or_default();
                Err(TempoError::Status(status, body))
            }
            _ => {
                let created: Value = response.json().await?;
                mirrored_jira_id(&created).ok_or_else(|| {
                    TempoError::Status(StatusCode::OK, "the answer has no jiraWorklogId".to_owned())
                })
            }
        }
    }
}

// The one place that decides how a worklog posted to Tempo is told apart when
// it comes back through the Jira poll. Constraint: UNVERIFIED against a real
// Tempo (the spike SHR-20 has no findings). It is assumed that the create
// response carries the id of the mirrored Jira worklog as `jiraWorklogId`,
// and that this id is what Jira's worklog list returns later. If the spike
// shows otherwise, this function is the only thing to change.
fn mirrored_jira_id(created: &Value) -> Option<String> {
    match &created["jiraWorklogId"] {
        Value::Number(id) => Some(id.to_string()),
        Value::String(id) if !id.is_empty() => Some(id.clone()),
        _ => None,
    }
}
