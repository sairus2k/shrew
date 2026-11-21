---
type: C4 Component
title: Timer
status: stable
groma:
  id: timer
  parent: app
  code:
    - scanner: rust
      file: crates/app/src/timer.rs
  group: Time tracking
  technology: Rust, GPUI
description: 'The running timer: start and stop on an issue, and the elapsed time in the status bar'
---

At most one timer runs. Its start is stored by the core in SQLite, so it survives a restart; this component keeps a copy for rendering, ticks the display and sends every start and stop to the core in order. Stopping logs the elapsed time as a worklog. It provides the start/stop control inside the detail pane and the status-bar item that shows the running timer with any issue open.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/app/src/timer.rs](../../../../../../crates/app/src/timer.rs) | [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | Starts and stops timer | async call through the Core bridge |
