---
type: Groma Project
title: Shrew
groma:
  profile: architecture
description: A local-first macOS desktop client for Jira Cloud
---

Shrew lets a developer work on Jira Cloud issues from a native macOS window: search and read issues, change status and assignee, comment, and track time. Everything the user sees comes from a local SQLite cache and every change is written locally first, so the interface never waits for the network; a background sync engine exchanges the changes with Jira and, optionally, sends worklogs to Tempo.

The repository is a Cargo workspace of two crates that build into one process: `crates/app`, the GPUI binary, and `crates/core` (package `app-core`), a library with the database, the sync and the Jira and Tempo clients and no UI dependency.
