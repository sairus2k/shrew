---
type: C4 Component
title: ADF conversion
status: stable
groma:
  id: adf
  parent: app
  code:
    - scanner: rust
      file: crates/core/src/adf.rs
  group: Jira sync
  technology: Rust, serde_json
description: Converts Atlassian Document Format to Markdown and plain text, and plain text to ADF
---

Jira v3 sends descriptions and comments as ADF JSON. They are turned into Markdown for display and into plain text for the full-text index; comments and worklog comments written here go out as ADF built from plain text.
