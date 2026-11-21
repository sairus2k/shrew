//! The Tempo Timesheets REST API, used for creating worklogs only.

use reqwest::StatusCode;
use serde_json::Value;
use std::fmt;

#[derive(Debug)]
pub(crate) enum TempoError {
    /// The Tempo token is wrong or has expired (401), or it lacks the scope
    /// for the request (403).
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

    /// Creates the worklog and returns Tempo's id of it.
    pub(crate) async fn create_worklog(&self, body: &Value) -> Result<i64, TempoError> {
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
                // A token without the scope gets "This operation requires one
                // of the following scopes: worklogs:manage." Any other 403 is
                // about this worklog, such as no permission to log on the issue.
                if status == StatusCode::FORBIDDEN && body.contains("following scopes") {
                    return Err(TempoError::Unauthorized);
                }
                Err(TempoError::Status(status, body))
            }
            _ => {
                let created: Value = response.json().await?;
                created["tempoWorklogId"].as_i64().ok_or_else(|| {
                    TempoError::Status(
                        StatusCode::OK,
                        "the answer has no tempoWorklogId".to_owned(),
                    )
                })
            }
        }
    }
}
