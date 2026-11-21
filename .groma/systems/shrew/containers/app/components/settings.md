---
type: C4 Component
title: Settings screen
status: stable
groma:
  id: settings
  parent: app
  code:
    - scanner: rust
      file: crates/app/src/settings.rs
  group: Account and settings
  technology: Rust, GPUI
description: Chooses where logged time goes, Jira or Tempo, and saves the Tempo API token
---

Reads and changes the worklog target through the core. Tempo can only be chosen once a Tempo token is saved.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/app/src/settings.rs](../../../../../../crates/app/src/settings.rs) | [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | Sets worklog target | async call through the Core bridge |
