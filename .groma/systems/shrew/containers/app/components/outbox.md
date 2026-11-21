---
type: C4 Component
title: Outbox
status: stable
groma:
  id: outbox
  parent: app
  code:
    - scanner: rust
      file: crates/core/src/db/sync_queue.rs
  group: Local data
  technology: Rust, sqlx, SQLite
description: The queue of local changes Jira has not confirmed yet
---

The `sync_queue` table: transitions, assignee changes, comments and worklogs in the order the user made them. The Core API enqueues; the outbound sync claims the oldest entry, and marks it synced, releases it for a retry or marks it failed. Refused comments stay here until the user retries or discards them, and the count of unsent entries is what the sign-out warning shows.
