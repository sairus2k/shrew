---
type: Groma Flow
title: Log time to Tempo
groma:
  id: log-time-to-tempo
---

The developer makes Tempo the worklog target once; from then on time logged after the fact is queued locally and created in Tempo, which mirrors it into Jira.

## Steps

| From | To | Action |
| --- | --- | --- |
| [Developer](../actors/developer.md) | [Settings screen](../systems/shrew/containers/app/components/settings.md) | Saves the Tempo API token and chooses Tempo as the worklog target |
| [Settings screen](../systems/shrew/containers/app/components/settings.md) | [Core API](../systems/shrew/containers/app/components/core-api.md) | Calls save_tempo_token, then set_worklog_target |
| [Core API](../systems/shrew/containers/app/components/core-api.md) | [Credentials and config](../systems/shrew/containers/app/components/credentials.md) | Keeps the token without checking it and writes the target to the config file; Tempo is refused as the target while no token is saved |
| [Credentials and config](../systems/shrew/containers/app/components/credentials.md) | [macOS Keychain](../externals/macos-keychain.md) | Stores the Tempo token |
| [Developer](../actors/developer.md) | [Issue detail pane](../systems/shrew/containers/app/components/issue-detail.md) | Opens the log-time form of an issue |
| [Issue detail pane](../systems/shrew/containers/app/components/issue-detail.md) | [Worklog form](../systems/shrew/containers/app/components/worklog-form.md) | Shows the form |
| [Worklog form](../systems/shrew/containers/app/components/worklog-form.md) | [Time formatting](../systems/shrew/containers/app/components/time-format.md) | Parses the time the developer entered |
| [Worklog form](../systems/shrew/containers/app/components/worklog-form.md) | [Core API](../systems/shrew/containers/app/components/core-api.md) | Calls log_time |
| [Core API](../systems/shrew/containers/app/components/core-api.md) | [Worklog store](../systems/shrew/containers/app/components/worklog-store.md) | Logs the time as a pending worklog, shown on the issue at once |
| [Worklog store](../systems/shrew/containers/app/components/worklog-store.md) | [Outbox](../systems/shrew/containers/app/components/outbox.md) | Enqueues the worklog in the same transaction |
| [Core API](../systems/shrew/containers/app/components/core-api.md) | [Sync engine](../systems/shrew/containers/app/components/sync-engine.md) | Wakes the engine |
| [Sync engine](../systems/shrew/containers/app/components/sync-engine.md) | [Outbound sync](../systems/shrew/containers/app/components/outbound-sync.md) | Starts the delivery of the outbox |
| [Outbound sync](../systems/shrew/containers/app/components/outbound-sync.md) | [Tempo client](../systems/shrew/containers/app/components/tempo-client.md) | Posts the worklog, because Tempo is the target |
| [Tempo client](../systems/shrew/containers/app/components/tempo-client.md) | [Tempo](../externals/tempo.md) | Creates the worklog |
| [Outbound sync](../systems/shrew/containers/app/components/outbound-sync.md) | [Worklog store](../systems/shrew/containers/app/components/worklog-store.md) | Confirms the worklog with the id it was given |
