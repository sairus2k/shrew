---
type: C4 Component
title: Project picker
status: stable
groma:
  id: project-picker
  parent: app
  code:
    - scanner: rust
      file: crates/app/src/projects.rs
  group: Onboarding
  technology: Rust, GPUI
description: Chooses which Jira projects are kept locally
---

Lists the projects the account can see next to the ones already watched and saves the choice through the core. It is the second screen of a new install and can be opened again from the home screen; after saving, the core says whether newly added projects still have to be downloaded, and the shell continues with that step.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [crates/app/src/projects.rs](../../../../../../crates/app/src/projects.rs) | [crates/core/src/api.rs](../../../../../../crates/core/src/api.rs) | Saves watched projects | async call through the Core bridge |
