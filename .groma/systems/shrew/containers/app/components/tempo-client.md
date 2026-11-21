---
type: C4 Component
title: Tempo client
status: stable
groma:
  id: tempo-client
  parent: app
  code:
    - scanner: rust
      file: crates/core/src/sync/tempo_client.rs
  group: Jira sync
  technology: Rust, reqwest, Tempo REST API v4
description: The HTTP client for the Tempo REST API, used only to create worklogs
---

Posts one worklog with the Tempo bearer token and returns the id Tempo gave it. Maps answers to `TempoError`, which sets a rejected token apart from other refusals.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [tempo-client](tempo-client.md) | [tempo](../../../../../externals/tempo.md) | Creates worklogs | HTTPS, REST API v4 |
