---
type: C4 Component
title: Transition and assignee lookup
status: stable
groma:
  id: issue-options
  parent: app
  code:
    - scanner: rust
      file: crates/core/src/sync/transitions.rs
    - scanner: rust
      file: crates/core/src/sync/assignable.rs
    - scanner: rust
      file: crates/core/src/db/transitions.rs
    - scanner: rust
      file: crates/core/src/db/assignable.rs
  group: Jira sync
  technology: Rust, sqlx, SQLite
description: Serves the transitions and the assignable people of an issue from a local cache, asking Jira when needed
---

What the status menu and the assignee menu offer. An answer comes from the cache tables when there is one, so the menus open without a request; a stale entry is returned at once and refreshed from Jira in the background, and only a missing one waits for Jira. Transitions are cached per project, issue type and status rather than per issue. The outbound sync drops the entries of an issue whose change Jira refused.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/core/src/sync/transitions.rs](../../../../../../crates/core/src/sync/transitions.rs) | [crates/core/src/sync/jira_client.rs](../../../../../../crates/core/src/sync/jira_client.rs) | Fetches transitions | Rust call |
| [crates/core/src/sync/assignable.rs](../../../../../../crates/core/src/sync/assignable.rs) | [crates/core/src/sync/jira_client.rs](../../../../../../crates/core/src/sync/jira_client.rs) | Fetches assignable users | Rust call |
| [crates/core/src/sync/transitions.rs](../../../../../../crates/core/src/sync/transitions.rs) | [crates/core/src/db/issues.rs](../../../../../../crates/core/src/db/issues.rs) | Reads local status | sqlx |
