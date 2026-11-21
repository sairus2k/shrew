---
type: C4 Component
title: Credentials and config
status: stable
groma:
  id: credentials
  parent: app
  code:
    - scanner: rust
      file: crates/core/src/auth.rs
  group: Local data
  technology: Rust, serde_json, keyring
description: Loads and saves the account config file and the API tokens
---

The config (`config.json` in the data directory) holds the Jira site, the account, the watched and pending projects, the sync positions and the worklog target. The Jira and Tempo API tokens never go into that file: they are kept behind the `TokenStore` trait, in the macOS Keychain in the app and in plain files in tests. Also fixes the Tempo base URL, which is deliberately not a setting.

## Relationships

| Source | Target | Description | Technology |
| --- | --- | --- | --- |
| [credentials](credentials.md) | [macos-keychain](../../../../../externals/macos-keychain.md) | Stores API tokens | Keychain Services |
