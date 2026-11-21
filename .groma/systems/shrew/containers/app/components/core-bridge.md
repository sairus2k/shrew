---
type: C4 Component
title: Core bridge
status: stable
groma:
  id: core-bridge
  parent: app
  code:
    - scanner: rust
      file: crates/app/src/runtime.rs
  group: Application shell
  technology: Rust, GPUI, tokio
description: The only way from the UI into the core, and of core events back into the UI
---

Core futures need the tokio runtime and must not be awaited on a GPUI executor. `spawn` runs a Core API call on tokio with the shared state handle and gives the view a task it can await; every view goes through it. `SyncBridge` pumps the events the core sends over its channel into a GPUI entity, so that views subscribe to sync events like to any other entity and reload from the cache.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/app/src/runtime.rs](../../../../../../crates/app/src/runtime.rs) | [crates/core/src/events.rs](../../../../../../crates/core/src/events.rs) | Relays sync events | tokio mpsc channel |
