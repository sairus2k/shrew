---
type: C4 Component
title: Time formatting
status: stable
groma:
  id: time-format
  parent: app
  code:
    - scanner: rust
      file: crates/app/src/duration.rs
  group: Time tracking
  technology: Rust, chrono
description: How the app writes and reads durations and local times
---

Formats seconds as `1h 23m` and Unix time as a local `2026-03-29 10:15`, and parses both back with messages meant for the user. Used by the worklog form and the detail pane.
