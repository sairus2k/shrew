---
type: C4 Component
title: Issue detail pane
status: stable
groma:
  id: issue-detail
  parent: app
  code:
    - scanner: rust
      file: crates/app/src/issue_detail.rs
  group: Issues
  technology: Rust, GPUI
description: 'Shows one issue beside the list: fields, description, comments and worklogs'
---

Renders the issue held by the shared issue model: its fields, the Markdown description, the comments and the worklogs, including pending ones and the ones Jira or Tempo refused, which the user can retry or discard here. It hosts the controls that change the issue: the status menu, the assignee menu, the comment form, the worklog form and the timer control.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/app/src/issue_detail.rs](../../../../../../crates/app/src/issue_detail.rs) | [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | Retries refused changes | async call through the Core bridge |
| [crates/app/src/issue_detail.rs](../../../../../../crates/app/src/issue_detail.rs) | [crates/app/src/issue.rs](../../../../../../crates/app/src/issue.rs) | Renders shared issue | GPUI entity |
| [crates/app/src/issue_detail.rs](../../../../../../crates/app/src/issue_detail.rs) | [crates/app/src/transition.rs](../../../../../../crates/app/src/transition.rs) | Hosts status menu | GPUI entity |
| [crates/app/src/issue_detail.rs](../../../../../../crates/app/src/issue_detail.rs) | [crates/app/src/assignee.rs](../../../../../../crates/app/src/assignee.rs) | Hosts assignee menu | GPUI entity |
| [crates/app/src/issue_detail.rs](../../../../../../crates/app/src/issue_detail.rs) | [crates/app/src/comment.rs](../../../../../../crates/app/src/comment.rs) | Hosts comment form | GPUI entity |
| [crates/app/src/issue_detail.rs](../../../../../../crates/app/src/issue_detail.rs) | [crates/app/src/worklog.rs](../../../../../../crates/app/src/worklog.rs) | Hosts worklog form | GPUI entity |
| [crates/app/src/issue_detail.rs](../../../../../../crates/app/src/issue_detail.rs) | [crates/app/src/timer.rs](../../../../../../crates/app/src/timer.rs) | Hosts timer control | GPUI entity |
| [crates/app/src/issue_detail.rs](../../../../../../crates/app/src/issue_detail.rs) | [crates/app/src/duration.rs](../../../../../../crates/app/src/duration.rs) | Formats times | Rust call |
