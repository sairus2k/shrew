---
type: C4 Component
title: Comment form
status: stable
groma:
  id: comment-form
  parent: app
  code:
    - scanner: rust
      file: crates/app/src/comment.rs
  group: Issues
  technology: Rust, GPUI
description: The input under the comments of an issue
---

Hands the text to the core, which queues it; the comment then appears in the issue as pending until Jira has it.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/app/src/comment.rs](../../../../../../crates/app/src/comment.rs) | [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | Queues comment | async call through the Core bridge |
