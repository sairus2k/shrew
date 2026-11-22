---
type: C4 Component
title: Issue record
status: stable
groma:
  id: issue-record
  parent: app
  code:
    - scanner: rust
      file: crates/core/src/sync/issue_record.rs
  group: Jira sync
  technology: Rust, serde_json
description: Maps a Jira issue to a cache record, shared by the inbound and the outbound sync
---
