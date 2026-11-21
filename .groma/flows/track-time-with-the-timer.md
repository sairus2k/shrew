---
type: Groma Flow
title: Track time with the timer
groma:
  id: track-time-with-the-timer
---

A stopped timer becomes a worklog that is delivered to Jira or, when Tempo is the worklog target, to Tempo.

## Steps

| From | To | Action |
| --- | --- | --- |
| [Developer](../actors/developer.md) | [Timer](../systems/shrew/containers/app/components/timer.md) | Starts the timer on an issue and later stops it |
| [Timer](../systems/shrew/containers/app/components/timer.md) | [Core API](../systems/shrew/containers/app/components/core-api.md) | Calls start_timer, then stop_timer with the moment of the click |
| [Core API](../systems/shrew/containers/app/components/core-api.md) | [Timer store](../systems/shrew/containers/app/components/timer-store.md) | Stores the start; on stop removes the timer |
| [Timer store](../systems/shrew/containers/app/components/timer-store.md) | [Worklog store](../systems/shrew/containers/app/components/worklog-store.md) | Logs the elapsed time as a pending worklog |
| [Worklog store](../systems/shrew/containers/app/components/worklog-store.md) | [Outbox](../systems/shrew/containers/app/components/outbox.md) | Enqueues the worklog for delivery |
| [Core API](../systems/shrew/containers/app/components/core-api.md) | [Sync engine](../systems/shrew/containers/app/components/sync-engine.md) | Wakes the engine |
| [Sync engine](../systems/shrew/containers/app/components/sync-engine.md) | [Outbound sync](../systems/shrew/containers/app/components/outbound-sync.md) | Starts the delivery of the outbox |
| [Outbound sync](../systems/shrew/containers/app/components/outbound-sync.md) | [Tempo client](../systems/shrew/containers/app/components/tempo-client.md) | Posts the worklog when Tempo is the target; otherwise it goes through the Jira client |
| [Tempo client](../systems/shrew/containers/app/components/tempo-client.md) | [Tempo](../externals/tempo.md) | Creates the worklog |
| [Outbound sync](../systems/shrew/containers/app/components/outbound-sync.md) | [Worklog store](../systems/shrew/containers/app/components/worklog-store.md) | Confirms the worklog with the id it was given |
