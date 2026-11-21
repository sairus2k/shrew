---
type: Groma Flow
title: Change an issue status
groma:
  id: change-an-issue-status
---

A write is local first: the issue shows its new status at once and Jira hears of it from the outbox. The same path carries assignee changes and comments.

## Steps

| From | To | Action |
| --- | --- | --- |
| [Developer](../actors/developer.md) | [Issue detail pane](../systems/shrew/containers/app/components/issue-detail.md) | Opens the status menu of the issue and picks a transition |
| [Issue detail pane](../systems/shrew/containers/app/components/issue-detail.md) | [Status menu](../systems/shrew/containers/app/components/status-menu.md) | Shows the transitions out of the current status |
| [Status menu](../systems/shrew/containers/app/components/status-menu.md) | [Issue model](../systems/shrew/containers/app/components/issue-model.md) | Shows the new status as pending before anything is stored |
| [Status menu](../systems/shrew/containers/app/components/status-menu.md) | [Core API](../systems/shrew/containers/app/components/core-api.md) | Calls transition_issue |
| [Core API](../systems/shrew/containers/app/components/core-api.md) | [Issue store](../systems/shrew/containers/app/components/issue-store.md) | Sets the pending status of the cached issue |
| [Core API](../systems/shrew/containers/app/components/core-api.md) | [Outbox](../systems/shrew/containers/app/components/outbox.md) | Enqueues the transition in the same transaction |
| [Core API](../systems/shrew/containers/app/components/core-api.md) | [Sync engine](../systems/shrew/containers/app/components/sync-engine.md) | Wakes the engine before its poll interval is over |
| [Sync engine](../systems/shrew/containers/app/components/sync-engine.md) | [Outbound sync](../systems/shrew/containers/app/components/outbound-sync.md) | Starts a cycle with the delivery of the outbox |
| [Outbound sync](../systems/shrew/containers/app/components/outbound-sync.md) | [Outbox](../systems/shrew/containers/app/components/outbox.md) | Claims the oldest entry |
| [Outbound sync](../systems/shrew/containers/app/components/outbound-sync.md) | [Jira client](../systems/shrew/containers/app/components/jira-client.md) | Sends the transition |
| [Jira client](../systems/shrew/containers/app/components/jira-client.md) | [Jira Cloud](../externals/jira-cloud.md) | Posts the transition to the issue |
| [Outbound sync](../systems/shrew/containers/app/components/outbound-sync.md) | [Issue store](../systems/shrew/containers/app/components/issue-store.md) | Confirms the issue, or puts back what Jira has if Jira refused |
| [Sync engine](../systems/shrew/containers/app/components/sync-engine.md) | [Core API](../systems/shrew/containers/app/components/core-api.md) | Announces the changed issue as a sync event |
| [Core bridge](../systems/shrew/containers/app/components/core-bridge.md) | [Core API](../systems/shrew/containers/app/components/core-api.md) | Re-emits the event inside GPUI |
| [Issue model](../systems/shrew/containers/app/components/issue-model.md) | [Core bridge](../systems/shrew/containers/app/components/core-bridge.md) | Hears that its issue changed |
| [Issue model](../systems/shrew/containers/app/components/issue-model.md) | [Core API](../systems/shrew/containers/app/components/core-api.md) | Reloads the issue from the cache |
