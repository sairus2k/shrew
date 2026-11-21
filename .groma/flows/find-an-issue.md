---
type: Groma Flow
title: Find an issue
groma:
  id: find-an-issue
---

The search runs over the local cache only: every change of the search field is one full-text query, so the list answers at once and works offline.

## Steps

| From | To | Action |
| --- | --- | --- |
| [Developer](../actors/developer.md) | [Issue list](../systems/shrew/containers/app/components/issue-list.md) | Types at least two characters in the search field, with or without a project filter |
| [Issue list](../systems/shrew/containers/app/components/issue-list.md) | [Core API](../systems/shrew/containers/app/components/core-api.md) | Calls search_issues on every change of the field, without a debounce; under two characters it calls list_issues |
| [Core API](../systems/shrew/containers/app/components/core-api.md) | [Issue store](../systems/shrew/containers/app/components/issue-store.md) | Runs the FTS5 query over keys, summaries and descriptions and returns the 50 best matches |
| [Issue list](../systems/shrew/containers/app/components/issue-list.md) | [Core bridge](../systems/shrew/containers/app/components/core-bridge.md) | Runs the same query again when a sync reports changed issues |
| [Developer](../actors/developer.md) | [Issue list](../systems/shrew/containers/app/components/issue-list.md) | Picks an issue from the matches |
| [App shell](../systems/shrew/containers/app/components/app-shell.md) | [Issue detail pane](../systems/shrew/containers/app/components/issue-detail.md) | Opens the chosen issue beside the list |
