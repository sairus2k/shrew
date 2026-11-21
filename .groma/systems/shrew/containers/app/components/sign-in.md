---
type: C4 Component
title: Sign-in screen
status: stable
groma:
  id: sign-in
  parent: app
  code:
    - scanner: rust
      file: crates/app/src/sign_in.rs
  group: Onboarding
  technology: Rust, GPUI
description: 'The first screen of a new install: site URL, email and API token'
---

Collects the Jira credentials and hands them to the core, which checks them with Jira and saves them only if Jira accepts them. Shows the core's error in the user's words otherwise, and reports to the shell when the user is signed in.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/app/src/sign_in.rs](../../../../../../crates/app/src/sign_in.rs) | [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | Signs in | async call through the Core bridge |
