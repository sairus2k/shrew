---
type: C4 Component
title: Issue model
status: stable
groma:
  id: issue-model
  parent: app
  code:
    - scanner: rust
      file: crates/app/src/issue.rs
  group: Issues
  technology: Rust, GPUI
description: One open issue as the UI sees it, shared by the views that show it
---

Holds a copy of the issue detail read from the cache and reloads it whenever the core reports a change to that issue. The status and assignee menus apply their change to it at once, before the core has stored it, so the pane never shows a stale value while the write is under way.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/app/src/issue.rs](../../../../../../crates/app/src/issue.rs) | [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | Loads issue detail | async call through the Core bridge |
| [crates/app/src/issue.rs](../../../../../../crates/app/src/issue.rs) | [crates/app/src/runtime.rs](../../../../../../crates/app/src/runtime.rs) | Reloads on changes | GPUI event subscription |
