---
type: C4 Actor
title: Developer
description: A person who works on Jira issues from their Mac
status: stable
groma:
  id: developer
---

Uses Shrew instead of the Jira web interface for the daily work on issues: finds and reads issues of the projects they watch, changes status and assignee, writes comments, and tracks time with a timer or by logging it after the fact. Signs in once with a Jira Cloud site URL, an email and an API token, and optionally saves a Tempo API token so that logged time goes to Tempo.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [developer](developer.md) | [sign-in](../systems/shrew/containers/app/components/sign-in.md) | Signs in | GPUI window |
| [developer](developer.md) | [project-picker](../systems/shrew/containers/app/components/project-picker.md) | Chooses watched projects | GPUI window |
| [developer](developer.md) | [issue-list](../systems/shrew/containers/app/components/issue-list.md) | Finds issues | GPUI window |
| [developer](developer.md) | [issue-detail](../systems/shrew/containers/app/components/issue-detail.md) | Reads and updates issues | GPUI window |
| [developer](developer.md) | [timer](../systems/shrew/containers/app/components/timer.md) | Tracks time | GPUI window |
| [developer](developer.md) | [settings](../systems/shrew/containers/app/components/settings.md) | Chooses worklog target | GPUI window |
| [developer](developer.md) | [new-token](../systems/shrew/containers/app/components/new-token.md) | Enters new token | GPUI window |
| [developer](developer.md) | [app-shell](../systems/shrew/containers/app/components/app-shell.md) | Opens the app | macOS app |
