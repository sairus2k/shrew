---
type: C4 Component
title: Timer store
status: stable
groma:
  id: timer-store
  parent: app
  code:
    - scanner: rust
      file: crates/core/src/db/timer.rs
  group: Local data
  technology: Rust, sqlx, SQLite
description: The running timer, kept in SQLite so that it survives a restart or a crash
---

Stores at most one running timer as its issue and start time. Stopping it logs the elapsed time through the worklog store in the same transaction. Follows an issue that Jira renamed and drops the timer of an issue that is gone.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/core/src/db/timer.rs](../../../../../../crates/core/src/db/timer.rs) | [crates/core/src/db/worklogs.rs](../../../../../../crates/core/src/db/worklogs.rs) | Logs stopped time | sqlx |
