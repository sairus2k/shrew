---
type: C4 Component
title: Menu bar item
status: stable
groma:
  id: tray
  parent: app
  code:
    - scanner: rust
      file: crates/app/src/tray.rs
  group: Application shell
  technology: Rust, GPUI, gpui-tray
description: 'The item in the macOS menu bar: an icon whose menu shows the running timer, stops it and leads back to the window'
---

Puts an item with the app's template icon into the macOS menu bar. While the timer runs, its menu starts with the issue key and the start of the timer and offers a stop; it always opens the app and quits it. Each command is a GPUI action with an app-wide handler. The item follows the timer the app shell hands it and keeps it alive, so the timer is seen and can be stopped while the window shows another screen or is closed. With no window, opening the app asks the core for the setup step and opens a new window.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/app/src/tray.rs](../../../../../../crates/app/src/tray.rs) | [crates/app/src/timer.rs](../../../../../../crates/app/src/timer.rs) | Shows the running timer and stops it | GPUI entity |
| [crates/app/src/tray.rs](../../../../../../crates/app/src/tray.rs) | [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | Asks for the setup step before it opens a new window | async call through the Core bridge |
