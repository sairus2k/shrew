---
type: C4 Component
title: Account control
status: stable
groma:
  id: account
  parent: app
  code:
    - scanner: rust
      file: crates/app/src/account.rs
  group: Account and settings
  technology: Rust, GPUI
description: Names the signed-in account in the status line and signs it out
---

Shows who is signed in. On sign-out it first asks the core how many changes have not reached Jira, warns that they will be discarded, and then has the core delete the credentials and the cache.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/app/src/account.rs](../../../../../../crates/app/src/account.rs) | [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | Signs out | async call through the Core bridge |
