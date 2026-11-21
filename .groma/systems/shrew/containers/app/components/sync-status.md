---
type: C4 Component
title: Sync status line
status: stable
groma:
  id: sync-status
  parent: app
  code:
    - scanner: rust
      file: crates/app/src/sync_status.rs
  group: Application shell
  technology: Rust, GPUI
description: Tells at the bottom of the home screen whether the issues on screen are current
---

Listens to the sync events and shows whether a sync is running, when the last one completed, whether the last one failed, and which issues the sync had to skip and why. Reads the time of the last sync from the core when it starts.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/app/src/sync_status.rs](../../../../../../crates/app/src/sync_status.rs) | [crates/app/src/runtime.rs](../../../../../../crates/app/src/runtime.rs) | Follows sync progress | GPUI event subscription |
| [crates/app/src/sync_status.rs](../../../../../../crates/app/src/sync_status.rs) | [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | Reads last sync time | async call through the Core bridge |
