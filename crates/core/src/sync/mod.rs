pub(crate) mod assignable;
pub(crate) mod engine;
pub(crate) mod inbound;
pub(crate) mod issue_record;
pub(crate) mod jira_client;
pub(crate) mod outbound;
pub(crate) mod tempo_client;
pub(crate) mod transitions;

/// A Jira where no worklog changed: every sync cycle asks for them, so a test
/// that runs one needs this unless it mounts its own.
#[cfg(test)]
pub(crate) fn no_worklog_changes() -> wiremock::Mock {
    use wiremock::{
        Mock, ResponseTemplate,
        matchers::{method, path_regex},
    };
    Mock::given(method("GET"))
        .and(path_regex("^/rest/api/3/worklog/(updated|deleted)$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({ "values": [], "until": 1_700_000_000_000_i64, "lastPage": true }),
        ))
}

/// A site without sprints: every sync cycle and every initial sync asks for
/// the sprint field, so a test that runs one needs this unless it mounts its
/// own, which has a higher priority.
#[cfg(test)]
pub(crate) fn no_sprint_field() -> wiremock::Mock {
    use wiremock::{
        Mock, ResponseTemplate,
        matchers::{method, path},
    };
    Mock::given(method("GET"))
        .and(path("/rest/api/3/field"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .with_priority(10)
}
