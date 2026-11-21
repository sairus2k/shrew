---
type: C4 Container
title: Desktop app
status: stable
groma:
  id: app
  parent: shrew
  technology: Rust, GPUI (gpui-kit), tokio, sqlx with SQLite, reqwest
description: 'The macOS application: the GPUI window and the core behind it, in one process'
---

A single native binary. The UI half (`crates/app`) renders the screens with GPUI on the main thread; the core half (`crates/core`, linked in as a library) owns all data and network work and runs on a tokio runtime. The two meet at a narrow seam: the UI calls the async functions of the Core API through the Core bridge and listens to the sync events the core emits, then re-reads what it shows. The UI cannot reach SQLite or the Jira client directly.

The core stores its data in an embedded SQLite database (`shrew.db`, WAL mode, schema in `crates/core/migrations`) that holds the cached issues with a full-text index, worklogs, the running timer, menu caches and the outbox of unsent changes. Account settings and sync positions are in `config.json` beside it; the API tokens are in the macOS Keychain. The database is embedded in this process and has no element of its own on the map.
