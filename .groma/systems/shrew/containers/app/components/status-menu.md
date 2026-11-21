---
type: C4 Component
title: Status menu
status: stable
groma:
  id: status-menu
  parent: app
  code:
    - scanner: rust
      file: crates/app/src/transition.rs
  group: Issues
  technology: Rust, GPUI
description: The status of an issue as a button whose menu holds the transitions out of it
---

Loads the available transitions through the core, asks in a small form for the fields Jira requires for a transition (a choice from Jira's allowed values, or text), shows the new status at once through the issue model and sends the transition to the core.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/app/src/transition.rs](../../../../../../crates/app/src/transition.rs) | [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | Queues status transition | async call through the Core bridge |
| [crates/app/src/transition.rs](../../../../../../crates/app/src/transition.rs) | [crates/app/src/issue.rs](../../../../../../crates/app/src/issue.rs) | Applies status optimistically | GPUI entity |
