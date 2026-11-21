---
type: Groma Flow
title: Set up a new install
groma:
  id: set-up-a-new-install
---

From an empty data directory to a filled cache: credentials checked with Jira, projects chosen, every issue of them downloaded.

## Steps

| From | To | Action |
| --- | --- | --- |
| [Developer](../actors/developer.md) | [Sign-in screen](../systems/shrew/containers/app/components/sign-in.md) | Enters the site URL, the email and the API token |
| [Sign-in screen](../systems/shrew/containers/app/components/sign-in.md) | [Core API](../systems/shrew/containers/app/components/core-api.md) | Calls sign_in |
| [Core API](../systems/shrew/containers/app/components/core-api.md) | [Jira client](../systems/shrew/containers/app/components/jira-client.md) | Checks the credentials |
| [Jira client](../systems/shrew/containers/app/components/jira-client.md) | [Jira Cloud](../externals/jira-cloud.md) | Asks who the token belongs to |
| [Core API](../systems/shrew/containers/app/components/core-api.md) | [Credentials and config](../systems/shrew/containers/app/components/credentials.md) | Saves the account once Jira accepted it |
| [Credentials and config](../systems/shrew/containers/app/components/credentials.md) | [macOS Keychain](../externals/macos-keychain.md) | Stores the API token |
| [Developer](../actors/developer.md) | [Project picker](../systems/shrew/containers/app/components/project-picker.md) | Chooses the projects to keep locally |
| [Project picker](../systems/shrew/containers/app/components/project-picker.md) | [Core API](../systems/shrew/containers/app/components/core-api.md) | Saves the watched projects |
| [Initial sync screen](../systems/shrew/containers/app/components/initial-sync.md) | [Core API](../systems/shrew/containers/app/components/core-api.md) | Starts the download |
| [Core API](../systems/shrew/containers/app/components/core-api.md) | [Inbound sync](../systems/shrew/containers/app/components/inbound-sync.md) | Runs the initial sync |
| [Inbound sync](../systems/shrew/containers/app/components/inbound-sync.md) | [Jira client](../systems/shrew/containers/app/components/jira-client.md) | Reads the issues of each project page by page |
| [Inbound sync](../systems/shrew/containers/app/components/inbound-sync.md) | [Issue store](../systems/shrew/containers/app/components/issue-store.md) | Stores every page |
| [Initial sync screen](../systems/shrew/containers/app/components/initial-sync.md) | [Core bridge](../systems/shrew/containers/app/components/core-bridge.md) | Shows the progress and learns that the download is complete |
| [App shell](../systems/shrew/containers/app/components/app-shell.md) | [Issue list](../systems/shrew/containers/app/components/issue-list.md) | Shows the home screen |
