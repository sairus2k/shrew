---
type: C4 Component
title: Sync state
status: stable
groma:
  id: sync-state
  parent: app
  code:
    - scanner: rust
      file: crates/core/src/db/sync_state.rs
  group: Local data
  technology: Rust, sqlx, SQLite
description: How far the polls have read Jira and which projects they read, kept in SQLite beside the rows it describes
---

Holds the sync positions (the last issue poll, the last reconcile pass, the two worklog polls) in a one-row table and the watched projects with their pending flag in another. A poll moves its position in the transaction of the page it stored, and a change of the watched projects is committed with the deletion of the removed projects' rows, so the state and the cache cannot part. Takes over the state that earlier builds kept in config.json.
