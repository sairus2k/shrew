---
type: C4 Component
title: Inbound sync
status: stable
groma:
  id: inbound-sync
  parent: app
  code:
    - scanner: rust
      file: crates/core/src/sync/inbound.rs
  group: Jira sync
  technology: Rust, tokio, serde_json
description: Brings issues and worklogs from Jira into the local cache
---

Three jobs. The initial sync downloads every issue of the watched (or newly added) projects page by page and reports progress as events. The incremental poll asks Jira for the issues updated since the last sync and for the worklogs changed or deleted since the saved positions, and stores them. The reconcile pass compares the issue ids Jira has with the cached ones and removes what was deleted, moved to an unwatched project or hidden from the user. It maps Jira's issue JSON to cache records, extracting plain text from ADF for the search index, saves the sync positions in the config, and skips an issue it cannot read with an event instead of failing the sync.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/core/src/sync/inbound.rs](../../../../../../crates/core/src/sync/inbound.rs) | [crates/core/src/sync/jira_client.rs](../../../../../../crates/core/src/sync/jira_client.rs) | Fetches issues and worklogs | Rust call |
| [crates/core/src/sync/inbound.rs](../../../../../../crates/core/src/sync/inbound.rs) | [crates/core/src/db/issues.rs](../../../../../../crates/core/src/db/issues.rs) | Stores issue records | sqlx |
| [crates/core/src/sync/inbound.rs](../../../../../../crates/core/src/sync/inbound.rs) | [crates/core/src/db/worklogs.rs](../../../../../../crates/core/src/db/worklogs.rs) | Stores remote worklogs | sqlx |
| [crates/core/src/sync/inbound.rs](../../../../../../crates/core/src/sync/inbound.rs) | [crates/core/src/db/timer.rs](../../../../../../crates/core/src/db/timer.rs) | Follows renamed issues | sqlx |
| [crates/core/src/sync/inbound.rs](../../../../../../crates/core/src/sync/inbound.rs) | [crates/core/src/auth.rs](../../../../../../crates/core/src/auth.rs) | Saves sync positions | Rust call |
| [crates/core/src/sync/inbound.rs](../../../../../../crates/core/src/sync/inbound.rs) | [crates/core/src/adf.rs](../../../../../../crates/core/src/adf.rs) | Extracts searchable text | Rust call |
| [crates/core/src/sync/inbound.rs](../../../../../../crates/core/src/sync/inbound.rs) | [crates/core/src/db/sync_queue.rs](../../../../../../crates/core/src/db/sync_queue.rs) | Re-keys or drops entries | sqlx |
