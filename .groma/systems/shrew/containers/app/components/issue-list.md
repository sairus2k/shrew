---
type: C4 Component
title: Issue list
status: stable
groma:
  id: issue-list
  parent: app
  code:
    - scanner: rust
      file: crates/app/src/issue_list.rs
  group: Issues
  technology: Rust, GPUI
description: 'The main screen: the cached issues, a project filter and the search over them'
---

Shows the issues of the watched projects from the local cache, filtered by project and by a full-text query, and reloads when the core reports changed issues. Picking a row tells the shell to open that issue in the detail pane.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/app/src/issue_list.rs](../../../../../../crates/app/src/issue_list.rs) | [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | Lists and searches issues | async call through the Core bridge |
| [crates/app/src/issue_list.rs](../../../../../../crates/app/src/issue_list.rs) | [crates/app/src/runtime.rs](../../../../../../crates/app/src/runtime.rs) | Reloads on changes | GPUI event subscription |
