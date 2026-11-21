---
type: C4 Component
title: Worklog form
status: stable
groma:
  id: worklog-form
  parent: app
  code:
    - scanner: rust
      file: crates/app/src/worklog.rs
  group: Time tracking
  technology: Rust, GPUI
description: Logs time on an issue after the fact
---

Takes a duration such as `1h 30m`, a start time and an optional comment, validates them and hands the worklog to the core.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/app/src/worklog.rs](../../../../../../crates/app/src/worklog.rs) | [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | Logs time | async call through the Core bridge |
| [crates/app/src/worklog.rs](../../../../../../crates/app/src/worklog.rs) | [crates/app/src/duration.rs](../../../../../../crates/app/src/duration.rs) | Parses entered time | Rust call |
