---
type: C4 Component
title: Issue store
status: stable
groma:
  id: issue-store
  parent: app
  code:
    - scanner: rust
      file: crates/core/src/db/issues.rs
  group: Local data
  technology: Rust, sqlx, SQLite FTS5
description: 'The cached issues: storing what Jira sends, the list, the detail and full-text search'
---

Reads and writes the `issues` table and its FTS5 index. The inbound sync upserts issue records here; the Core API reads the list, the search results and the detail, for which the stored ADF description and comments are rendered as Markdown and the comments still waiting in the outbox and the worklogs are added. It also carries the optimistic state of an issue: a status or assignee set here is marked pending until the outbound sync confirms it.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/core/src/db/issues.rs](../../../../../../crates/core/src/db/issues.rs) | [crates/core/src/adf.rs](../../../../../../crates/core/src/adf.rs) | Renders Markdown | Rust call |
| [crates/core/src/db/issues.rs](../../../../../../crates/core/src/db/issues.rs) | [crates/core/src/db/worklogs.rs](../../../../../../crates/core/src/db/worklogs.rs) | Reads issue worklogs | sqlx |
