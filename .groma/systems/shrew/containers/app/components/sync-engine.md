---
type: C4 Component
title: Sync engine
status: stable
groma:
  id: sync-engine
  parent: app
  code:
    - scanner: rust
      file: crates/core/src/sync/engine.rs
    - scanner: rust
      file: crates/core/src/sync/mod.rs
  group: Jira sync
  technology: Rust, tokio
description: The background loop that keeps the cache in step with Jira
---

Runs one sync cycle, waits, and repeats. A cycle first delivers the outbox and rolls back changes that could not be confirmed, then polls Jira for changed issues and for changed and deleted worklogs, and about once an hour reconciles issues that were deleted or moved. It announces `Started`, the changed issues, and `Completed` or the error; a 401 from Jira pauses it until the token is replaced.

The wait depends on attention: 3 minutes while the window is focused, 15 in the background, 30 once nobody has touched the Mac for 5 minutes (read from CoreGraphics). A local write or a regained focus after a longer interval wakes it at once. A sign-out cuts a running cycle short.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/core/src/sync/engine.rs](../../../../../../crates/core/src/sync/engine.rs) | [crates/core/src/sync/outbound.rs](../../../../../../crates/core/src/sync/outbound.rs) | Delivers outbox first | Rust call |
| [crates/core/src/sync/engine.rs](../../../../../../crates/core/src/sync/engine.rs) | [crates/core/src/sync/inbound.rs](../../../../../../crates/core/src/sync/inbound.rs) | Polls Jira changes | Rust call |
| [crates/core/src/sync/engine.rs](../../../../../../crates/core/src/sync/engine.rs) | [crates/core/src/events.rs](../../../../../../crates/core/src/events.rs) | Announces sync results | tokio mpsc channel |
| [crates/core/src/sync/engine.rs](../../../../../../crates/core/src/sync/engine.rs) | [crates/core/src/db/sync_state.rs](../../../../../../crates/core/src/db/sync_state.rs) | Reads where each poll of a cycle starts | sqlx |
