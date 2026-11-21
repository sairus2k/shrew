---
type: C4 Component
title: Initial sync screen
status: stable
groma:
  id: initial-sync
  parent: app
  code:
    - scanner: rust
      file: crates/app/src/initial_sync.rs
  group: Onboarding
  technology: Rust, GPUI
description: Shows the first download of the watched projects with its progress
---

The only sync the user ever watches. It starts the download through the core, shows the per-project progress the core reports as events, offers a retry after an error, and reports to the shell when every issue is stored.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/app/src/initial_sync.rs](../../../../../../crates/app/src/initial_sync.rs) | [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | Starts initial sync | async call through the Core bridge |
| [crates/app/src/initial_sync.rs](../../../../../../crates/app/src/initial_sync.rs) | [crates/app/src/runtime.rs](../../../../../../crates/app/src/runtime.rs) | Follows download progress | GPUI event subscription |
