---
type: C4 Component
title: Outbound sync
status: stable
groma:
  id: outbound-sync
  parent: app
  code:
    - scanner: rust
      file: crates/core/src/sync/outbound.rs
  group: Jira sync
  technology: Rust, tokio
description: Delivers the outbox to Jira and Tempo and settles what they refuse
---

Claims outbox entries oldest first and sends each one: transitions, assignee changes and comments to Jira, worklogs to Jira or to Tempo depending on the chosen target. On success it confirms the entry and the cached row in one transaction. A transient failure is retried with backoff; a refused transition or assignee change is rolled back to what Jira has, while a refused comment or worklog stays visible as failed for the user to retry or discard, and each refusal is announced as an event. A rejected Tempo token holds back only the worklogs.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/core/src/sync/outbound.rs](../../../../../../crates/core/src/sync/outbound.rs) | [crates/core/src/db/sync_queue.rs](../../../../../../crates/core/src/db/sync_queue.rs) | Claims and settles entries | sqlx |
| [crates/core/src/sync/outbound.rs](../../../../../../crates/core/src/sync/outbound.rs) | [crates/core/src/sync/jira_client.rs](../../../../../../crates/core/src/sync/jira_client.rs) | Sends queued changes | Rust call |
| [crates/core/src/sync/outbound.rs](../../../../../../crates/core/src/sync/outbound.rs) | [crates/core/src/sync/tempo_client.rs](../../../../../../crates/core/src/sync/tempo_client.rs) | Posts worklogs | Rust call |
| [crates/core/src/sync/outbound.rs](../../../../../../crates/core/src/sync/outbound.rs) | [crates/core/src/db/issues.rs](../../../../../../crates/core/src/db/issues.rs) | Confirms or rolls back | sqlx |
| [crates/core/src/sync/outbound.rs](../../../../../../crates/core/src/sync/outbound.rs) | [crates/core/src/db/worklogs.rs](../../../../../../crates/core/src/db/worklogs.rs) | Confirms sent worklogs | sqlx |
| [crates/core/src/sync/outbound.rs](../../../../../../crates/core/src/sync/outbound.rs) | [crates/core/src/adf.rs](../../../../../../crates/core/src/adf.rs) | Builds comment bodies | Rust call |
| [crates/core/src/sync/outbound.rs](../../../../../../crates/core/src/sync/outbound.rs) | [crates/core/src/db/transitions.rs](../../../../../../crates/core/src/db/transitions.rs) | Forgets refused options | sqlx |
