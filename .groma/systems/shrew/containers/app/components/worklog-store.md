---
type: C4 Component
title: Worklog store
status: stable
groma:
  id: worklog-store
  parent: app
  code:
    - scanner: rust
      file: crates/core/src/db/worklogs.rs
  group: Local data
  technology: Rust, sqlx, SQLite, chrono
description: 'The worklogs of the cached issues: the ones logged here and the ones Jira has'
---

Logging time inserts a pending worklog and its outbox entry together. The outbound sync confirms it with the id Jira or Tempo gave, or marks it failed; the inbound sync upserts and deletes the worklogs Jira reports. Also converts start times between the stored form and Jira's.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/core/src/db/worklogs.rs](../../../../../../crates/core/src/db/worklogs.rs) | [crates/core/src/db/sync_queue.rs](../../../../../../crates/core/src/db/sync_queue.rs) | Enqueues worklog delivery | sqlx |
