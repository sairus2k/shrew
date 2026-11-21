---
type: Groma Flow
title: Start the app on a set-up install
groma:
  id: start-the-app-on-a-set-up-install
---

With an account, watched projects and a filled cache the app shows the cached issues before Jira has answered, and the first sync cycle catches up in the background.

## Steps

| From | To | Action |
| --- | --- | --- |
| [Developer](../actors/developer.md) | [App shell](../systems/shrew/containers/app/components/app-shell.md) | Opens the app |
| [App shell](../systems/shrew/containers/app/components/app-shell.md) | [Core startup and state](../systems/shrew/containers/app/components/core-startup.md) | Starts the core inside the tokio runtime |
| [Core startup and state](../systems/shrew/containers/app/components/core-startup.md) | [Database setup](../systems/shrew/containers/app/components/database.md) | Opens shrew.db and applies the migrations it does not have yet |
| [Core startup and state](../systems/shrew/containers/app/components/core-startup.md) | [Credentials and config](../systems/shrew/containers/app/components/credentials.md) | Loads the saved account from the config file |
| [Credentials and config](../systems/shrew/containers/app/components/credentials.md) | [macOS Keychain](../externals/macos-keychain.md) | Reads the Jira and the Tempo token |
| [Core startup and state](../systems/shrew/containers/app/components/core-startup.md) | [Sync engine](../systems/shrew/containers/app/components/sync-engine.md) | Spawns the sync loop |
| [App shell](../systems/shrew/containers/app/components/app-shell.md) | [Core API](../systems/shrew/containers/app/components/core-api.md) | Reads the setup step, which is Ready |
| [App shell](../systems/shrew/containers/app/components/app-shell.md) | [Core bridge](../systems/shrew/containers/app/components/core-bridge.md) | Subscribes to the sync events |
| [App shell](../systems/shrew/containers/app/components/app-shell.md) | [Issue list](../systems/shrew/containers/app/components/issue-list.md) | Shows the home screen |
| [Issue list](../systems/shrew/containers/app/components/issue-list.md) | [Core API](../systems/shrew/containers/app/components/core-api.md) | Lists the cached issues without waiting for Jira |
| [Sync engine](../systems/shrew/containers/app/components/sync-engine.md) | [Outbound sync](../systems/shrew/containers/app/components/outbound-sync.md) | Delivers what the outbox still holds from the last run |
| [Sync engine](../systems/shrew/containers/app/components/sync-engine.md) | [Inbound sync](../systems/shrew/containers/app/components/inbound-sync.md) | Polls what changed since the saved positions and, on this first cycle, reconciles removed issues |
| [Sync engine](../systems/shrew/containers/app/components/sync-engine.md) | [Core API](../systems/shrew/containers/app/components/core-api.md) | Announces the changed issues |
| [Issue list](../systems/shrew/containers/app/components/issue-list.md) | [Core bridge](../systems/shrew/containers/app/components/core-bridge.md) | Reloads the list |
