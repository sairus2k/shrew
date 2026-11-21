---
type: C4 Component
title: Jira client
status: stable
groma:
  id: jira-client
  parent: app
  code:
    - scanner: rust
      file: crates/core/src/sync/jira_client.rs
  group: Jira sync
  technology: Rust, reqwest, Jira Cloud REST API v3
description: The typed HTTP client for the Jira Cloud REST API
---

Every request to Jira goes through it: the account check, projects, JQL search pages and id lists, single issues, transitions, assignable users, comments and worklogs. It authenticates with the email and API token, pages through results, waits and retries when Jira rate-limits, and maps answers to `JiraError` so that callers can tell a rejected token from a refusal and from a network failure.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [jira-client](jira-client.md) | [jira-cloud](../../../../../externals/jira-cloud.md) | Reads and writes issues | HTTPS, REST API v3 |
