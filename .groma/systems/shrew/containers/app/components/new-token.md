---
type: C4 Component
title: New token banner
status: stable
groma:
  id: new-token
  parent: app
  code:
    - scanner: rust
      file: crates/app/src/new_token.rs
  group: Account and settings
  technology: Rust, GPUI
description: Asks for a new Jira API token after Jira rejected the saved one
---

Shown on the home screen from the moment the sync reports a rejected token. The core checks the entered token with Jira, saves it and resumes the sync; the banner then goes away.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/app/src/new_token.rs](../../../../../../crates/app/src/new_token.rs) | [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | Renews Jira token | async call through the Core bridge |
