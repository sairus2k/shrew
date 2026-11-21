---
type: C4 Component
title: Core startup and state
status: stable
groma:
  id: core-startup
  parent: app
  code:
    - scanner: rust
      file: crates/core/src/lib.rs
    - scanner: rust
      file: crates/core/src/state.rs
      symbol: set_window_focused
  group: Core interface
  technology: Rust, tokio
description: Opens the core and holds the state handle everything in it shares
---

`start` creates the data directory, opens and migrates the database, loads the saved account, builds the `AppState` handle and spawns the sync engine; it returns the handle and the receiving end of the event channel to the UI. `AppState` is opaque to the UI: it carries the SQLite pool, the event sender, the current config, the token stores, the waker of the sync engine, the window-focus signal and the locks that keep a sign-out and a sync cycle apart. Test-only variants start the core with tokens in files and without the engine.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/core/src/lib.rs](../../../../../../crates/core/src/lib.rs) | [crates/core/src/db/mod.rs](../../../../../../crates/core/src/db/mod.rs) | Opens database | Rust call |
| [crates/core/src/lib.rs](../../../../../../crates/core/src/lib.rs) | [crates/core/src/auth.rs](../../../../../../crates/core/src/auth.rs) | Loads saved account | Rust call |
| [crates/core/src/lib.rs](../../../../../../crates/core/src/lib.rs) | [crates/core/src/sync/engine.rs](../../../../../../crates/core/src/sync/engine.rs) | Spawns sync loop | tokio task |
