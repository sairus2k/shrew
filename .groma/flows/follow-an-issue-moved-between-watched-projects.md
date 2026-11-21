---
type: Groma Flow
title: Follow an issue moved between watched projects
groma:
  id: follow-an-issue-moved-between-watched-projects
---

Jira gives an issue a new key when it moves to another project. The incremental poll recognises the issue by its id and moves everything stored under the old key to the new one in one transaction, so that queued changes reach the issue and stay shown on it.

## Steps

| From | To | Action |
| --- | --- | --- |
| [Sync engine](../systems/shrew/containers/app/components/sync-engine.md) | [Inbound sync](../systems/shrew/containers/app/components/inbound-sync.md) | Polls the issues updated since the last sync |
| [Inbound sync](../systems/shrew/containers/app/components/inbound-sync.md) | [Jira client](../systems/shrew/containers/app/components/jira-client.md) | Asks for the changed issues of the watched projects |
| [Jira client](../systems/shrew/containers/app/components/jira-client.md) | [Jira Cloud](../externals/jira-cloud.md) | Searches; the moved issue comes back with the same id and a new key |
| [Inbound sync](../systems/shrew/containers/app/components/inbound-sync.md) | [Issue store](../systems/shrew/containers/app/components/issue-store.md) | Reads the key the id is stored under, then upserts by id: the row gets the new key and keeps a status or assignee that is still unsent |
| [Inbound sync](../systems/shrew/containers/app/components/inbound-sync.md) | [Outbox](../systems/shrew/containers/app/components/outbox.md) | Re-keys every entry of the issue, after the upsert, which finds the unsent ones by the old key |
| [Inbound sync](../systems/shrew/containers/app/components/inbound-sync.md) | [Worklog store](../systems/shrew/containers/app/components/worklog-store.md) | Re-keys the worklogs of the issue |
| [Inbound sync](../systems/shrew/containers/app/components/inbound-sync.md) | [Timer store](../systems/shrew/containers/app/components/timer-store.md) | Re-keys a timer running on the issue |
| [Sync engine](../systems/shrew/containers/app/components/sync-engine.md) | [Core API](../systems/shrew/containers/app/components/core-api.md) | Announces both the new and the old key as changed |
| [Core bridge](../systems/shrew/containers/app/components/core-bridge.md) | [Core API](../systems/shrew/containers/app/components/core-api.md) | Re-emits the event inside GPUI |
| [Issue model](../systems/shrew/containers/app/components/issue-model.md) | [Core bridge](../systems/shrew/containers/app/components/core-bridge.md) | A view open under the old key hears that its issue changed |
| [Sync engine](../systems/shrew/containers/app/components/sync-engine.md) | [Outbound sync](../systems/shrew/containers/app/components/outbound-sync.md) | Starts the next cycle with the delivery of the outbox |
| [Outbound sync](../systems/shrew/containers/app/components/outbound-sync.md) | [Outbox](../systems/shrew/containers/app/components/outbox.md) | Claims the entries queued before the move |
| [Outbound sync](../systems/shrew/containers/app/components/outbound-sync.md) | [Jira client](../systems/shrew/containers/app/components/jira-client.md) | Sends each of them to the new key |
