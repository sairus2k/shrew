---
type: Groma Flow
title: Background sync cycle
groma:
  id: background-sync-cycle
---

What the sync engine does every few minutes without the user: send what is queued, then read what changed in Jira and tell the UI.

## Steps

| From | To | Action |
| --- | --- | --- |
| [Sync engine](../systems/shrew/containers/app/components/sync-engine.md) | [Outbound sync](../systems/shrew/containers/app/components/outbound-sync.md) | Delivers the outbox first, so that the poll reads what Jira made of it |
| [Sync engine](../systems/shrew/containers/app/components/sync-engine.md) | [Inbound sync](../systems/shrew/containers/app/components/inbound-sync.md) | Polls updated issues, changed and deleted worklogs, and hourly reconciles removed issues |
| [Inbound sync](../systems/shrew/containers/app/components/inbound-sync.md) | [Jira client](../systems/shrew/containers/app/components/jira-client.md) | Asks for what changed since the saved positions |
| [Jira client](../systems/shrew/containers/app/components/jira-client.md) | [Jira Cloud](../externals/jira-cloud.md) | Searches issues and reads worklogs |
| [Inbound sync](../systems/shrew/containers/app/components/inbound-sync.md) | [Issue store](../systems/shrew/containers/app/components/issue-store.md) | Upserts the changed issues and deletes the removed ones |
| [Inbound sync](../systems/shrew/containers/app/components/inbound-sync.md) | [Worklog store](../systems/shrew/containers/app/components/worklog-store.md) | Stores the changed worklogs and deletes the removed ones |
| [Inbound sync](../systems/shrew/containers/app/components/inbound-sync.md) | [Credentials and config](../systems/shrew/containers/app/components/credentials.md) | Saves the new sync positions |
| [Sync engine](../systems/shrew/containers/app/components/sync-engine.md) | [Core API](../systems/shrew/containers/app/components/core-api.md) | Announces the changed issues and the end of the cycle |
| [Core bridge](../systems/shrew/containers/app/components/core-bridge.md) | [Core API](../systems/shrew/containers/app/components/core-api.md) | Re-emits the events inside GPUI |
| [Issue list](../systems/shrew/containers/app/components/issue-list.md) | [Core bridge](../systems/shrew/containers/app/components/core-bridge.md) | Hears that issues changed |
| [Issue list](../systems/shrew/containers/app/components/issue-list.md) | [Core API](../systems/shrew/containers/app/components/core-api.md) | Reloads the list from the cache |
