---
type: C4 Component
title: Assignee menu
status: stable
groma:
  id: assignee-menu
  parent: app
  code:
    - scanner: rust
      file: crates/app/src/assignee.rs
  group: Issues
  technology: Rust, GPUI
description: The assignee of an issue as a button whose menu holds the people it can be assigned to
---

Loads the assignable people and the signed-in account through the core, shows the choice at once through the issue model and sends the change to the core.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/app/src/assignee.rs](../../../../../../crates/app/src/assignee.rs) | [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | Queues assignee change | async call through the Core bridge |
| [crates/app/src/assignee.rs](../../../../../../crates/app/src/assignee.rs) | [crates/app/src/issue.rs](../../../../../../crates/app/src/issue.rs) | Applies assignee optimistically | GPUI entity |
