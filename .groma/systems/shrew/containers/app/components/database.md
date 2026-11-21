---
type: C4 Component
title: Database setup
status: stable
groma:
  id: database
  parent: app
  code:
    - scanner: rust
      file: crates/core/src/db/mod.rs
  group: Local data
  technology: Rust, sqlx, SQLite
description: Opens the SQLite file, applies the migrations and clears the cache on sign-out
---

Opens `shrew.db` in the data directory in WAL mode, so that views keep reading while the sync writes, and runs the embedded migrations from `crates/core/migrations`. `clear` deletes everything that came from Jira when the account changes. The tables themselves are read and written by the stores beside it; queries are checked at compile time against the cache in `.sqlx`.
