---
type: C4 Component
title: Core API
status: stable
groma:
  id: core-api
  parent: app
  code:
    - scanner: rust
      file: crates/core/src/api.rs
    - scanner: rust
      file: crates/core/src/models.rs
    - scanner: rust
      file: crates/core/src/events.rs
      symbol: rejection
  group: Core interface
  technology: Rust, tokio, sqlx
description: The functions, data types and events through which the UI uses the core
---

The whole contract between the UI and the core. `api` holds the async functions the UI calls: account and setup (sign in and out, renew the token, watched projects, initial sync, worklog target and Tempo token), reads from the cache (issue list, full-text search, issue detail, transitions, assignable people, running timer) and writes (transition, assign, comment, log time, start and stop the timer, retry or discard a refused comment or worklog). `models` defines the data that crosses the seam and `events` the `SyncEvent`s the core announces.

A write never waits for Jira: in one SQLite transaction it changes the cached row and adds an entry to the outbox, then announces the changed issue and wakes the sync engine. Sign-in and token renewal are the exception: they ask Jira first and save only what Jira accepts.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | [crates/core/src/auth.rs](../../../../../../crates/core/src/auth.rs) | Saves account config | Rust call |
| [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | [crates/core/src/db/mod.rs](../../../../../../crates/core/src/db/mod.rs) | Clears cache on sign-out | sqlx |
| [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | [crates/core/src/db/issues.rs](../../../../../../crates/core/src/db/issues.rs) | Reads and marks issues | sqlx |
| [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | [crates/core/src/db/sync_queue.rs](../../../../../../crates/core/src/db/sync_queue.rs) | Enqueues local changes | sqlx |
| [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | [crates/core/src/db/worklogs.rs](../../../../../../crates/core/src/db/worklogs.rs) | Logs worklogs | sqlx |
| [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | [crates/core/src/db/timer.rs](../../../../../../crates/core/src/db/timer.rs) | Starts and stops timer | sqlx |
| [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | [crates/core/src/sync/inbound.rs](../../../../../../crates/core/src/sync/inbound.rs) | Runs initial sync | Rust call |
| [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | [crates/core/src/sync/jira_client.rs](../../../../../../crates/core/src/sync/jira_client.rs) | Verifies credentials | Rust call |
| [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | [crates/core/src/sync/transitions.rs](../../../../../../crates/core/src/sync/transitions.rs) | Gets transitions | Rust call |
| [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | [crates/core/src/sync/assignable.rs](../../../../../../crates/core/src/sync/assignable.rs) | Gets assignable people | Rust call |
| [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | [crates/core/src/sync/engine.rs](../../../../../../crates/core/src/sync/engine.rs) | Wakes after writes | tokio watch channel |
| [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | [crates/core/src/db/sync_state.rs](../../../../../../crates/core/src/db/sync_state.rs) | Saves the watched projects and reads the setup step | sqlx |
