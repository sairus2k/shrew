---
type: C4 Component
title: App shell
status: stable
groma:
  id: app-shell
  parent: app
  code:
    - scanner: rust
      file: crates/app/src/main.rs
    - scanner: rust
      file: crates/app/src/menu.rs
      symbol: init
  group: Application shell
  technology: Rust, GPUI
description: Starts the app, owns the window and decides which screen is shown
---

The entry point. It starts the tokio runtime and the core, opens the window and asks the core which setup step applies, then shows the matching screen: sign-in, project choice, initial sync, or the home screen with the issue list, the detail pane, the status line and the running timer. Each screen reports when it is done and the shell puts the next one in its place.

It also turns core events into notifications (sync errors, changes Jira refused, a rejected Tempo token), shows the banner that asks for a new Jira token, and tells the core when the window gains or loses focus, which sets the poll interval.

At startup it installs the application menu of the macOS menu bar: Hide Shrew, Hide Others, Show All and Quit Shrew, with the standard shortcuts, which also work while a text input has focus.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/app/src/main.rs](../../../../../../crates/app/src/main.rs) | [crates/core/src/lib.rs](../../../../../../crates/core/src/lib.rs) | Starts the core | Rust call |
| [crates/app/src/main.rs](../../../../../../crates/app/src/main.rs) | [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | Reads setup step | Rust call |
| [crates/app/src/main.rs](../../../../../../crates/app/src/main.rs) | [crates/core/src/state.rs](../../../../../../crates/core/src/state.rs) | Reports window focus | tokio watch channel |
| [crates/app/src/main.rs](../../../../../../crates/app/src/main.rs) | [crates/app/src/runtime.rs](../../../../../../crates/app/src/runtime.rs) | Receives sync events | GPUI event subscription |
| [crates/app/src/main.rs](../../../../../../crates/app/src/main.rs) | [crates/app/src/theme.rs](../../../../../../crates/app/src/theme.rs) | Installs palette | Rust call |
| [crates/app/src/main.rs](../../../../../../crates/app/src/main.rs) | [crates/app/src/sign_in.rs](../../../../../../crates/app/src/sign_in.rs) | Shows during setup | GPUI entity |
| [crates/app/src/main.rs](../../../../../../crates/app/src/main.rs) | [crates/app/src/projects.rs](../../../../../../crates/app/src/projects.rs) | Shows project choice | GPUI entity |
| [crates/app/src/main.rs](../../../../../../crates/app/src/main.rs) | [crates/app/src/initial_sync.rs](../../../../../../crates/app/src/initial_sync.rs) | Shows first download | GPUI entity |
| [crates/app/src/main.rs](../../../../../../crates/app/src/main.rs) | [crates/app/src/issue_list.rs](../../../../../../crates/app/src/issue_list.rs) | Shows home screen | GPUI entity |
| [crates/app/src/main.rs](../../../../../../crates/app/src/main.rs) | [crates/app/src/issue_detail.rs](../../../../../../crates/app/src/issue_detail.rs) | Opens chosen issue | GPUI entity |
| [crates/app/src/main.rs](../../../../../../crates/app/src/main.rs) | [crates/app/src/issue.rs](../../../../../../crates/app/src/issue.rs) | Creates per opened issue | GPUI entity |
| [crates/app/src/main.rs](../../../../../../crates/app/src/main.rs) | [crates/app/src/sync_status.rs](../../../../../../crates/app/src/sync_status.rs) | Shows in status line | GPUI entity |
| [crates/app/src/main.rs](../../../../../../crates/app/src/main.rs) | [crates/app/src/account.rs](../../../../../../crates/app/src/account.rs) | Shows in status line | GPUI entity |
| [crates/app/src/main.rs](../../../../../../crates/app/src/main.rs) | [crates/app/src/timer.rs](../../../../../../crates/app/src/timer.rs) | Creates shared timer | GPUI entity |
| [crates/app/src/main.rs](../../../../../../crates/app/src/main.rs) | [crates/app/src/new_token.rs](../../../../../../crates/app/src/new_token.rs) | Shows on rejected token | GPUI entity |
| [crates/app/src/main.rs](../../../../../../crates/app/src/main.rs) | [crates/app/src/settings.rs](../../../../../../crates/app/src/settings.rs) | Opens settings | GPUI entity |
| [crates/app/src/main.rs](../../../../../../crates/app/src/main.rs) | [crates/app/src/keys.rs](../../../../../../crates/app/src/keys.rs) | Handles shortcut actions | GPUI action |
