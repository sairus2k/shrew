---
type: Groma Flow
title: Renew a rejected Jira token
groma:
  id: renew-a-rejected-jira-token
---

Jira answers 401 to a sync: the engine stops running cycles with that token, the home screen asks for a new one, and the sync resumes once Jira accepted it. The changes in the outbox wait meanwhile.

## Steps

| From | To | Action |
| --- | --- | --- |
| [Sync engine](../systems/shrew/containers/app/components/sync-engine.md) | [Inbound sync](../systems/shrew/containers/app/components/inbound-sync.md) | Polls Jira in a regular cycle |
| [Inbound sync](../systems/shrew/containers/app/components/inbound-sync.md) | [Jira client](../systems/shrew/containers/app/components/jira-client.md) | Asks for the issues that changed |
| [Jira client](../systems/shrew/containers/app/components/jira-client.md) | [Jira Cloud](../externals/jira-cloud.md) | Gets a 401 answer: the API token is wrong or has expired |
| [Sync engine](../systems/shrew/containers/app/components/sync-engine.md) | [Core API](../systems/shrew/containers/app/components/core-api.md) | Remembers the rejected token, runs no cycle with it and announces TokenRejected |
| [Core bridge](../systems/shrew/containers/app/components/core-bridge.md) | [Core API](../systems/shrew/containers/app/components/core-api.md) | Re-emits the event inside GPUI |
| [App shell](../systems/shrew/containers/app/components/app-shell.md) | [Core bridge](../systems/shrew/containers/app/components/core-bridge.md) | Hears that the token was rejected |
| [App shell](../systems/shrew/containers/app/components/app-shell.md) | [New token banner](../systems/shrew/containers/app/components/new-token.md) | Shows the banner on the home screen |
| [Developer](../actors/developer.md) | [New token banner](../systems/shrew/containers/app/components/new-token.md) | Enters a new API token |
| [New token banner](../systems/shrew/containers/app/components/new-token.md) | [Core API](../systems/shrew/containers/app/components/core-api.md) | Calls renew_token; the banner goes away when it succeeds |
| [Core API](../systems/shrew/containers/app/components/core-api.md) | [Jira client](../systems/shrew/containers/app/components/jira-client.md) | Checks the new token before anything is saved |
| [Jira client](../systems/shrew/containers/app/components/jira-client.md) | [Jira Cloud](../externals/jira-cloud.md) | Asks who the token belongs to |
| [Core API](../systems/shrew/containers/app/components/core-api.md) | [Credentials and config](../systems/shrew/containers/app/components/credentials.md) | Replaces only the token of the saved account |
| [Credentials and config](../systems/shrew/containers/app/components/credentials.md) | [macOS Keychain](../externals/macos-keychain.md) | Stores the new token |
| [Core API](../systems/shrew/containers/app/components/core-api.md) | [Sync engine](../systems/shrew/containers/app/components/sync-engine.md) | Wakes the engine, which syncs again because the config holds another token |
