---
type: C4 System
title: macOS Keychain
description: The system store that keeps the Jira and Tempo API tokens
status: stable
groma:
  id: macos-keychain
  technology: macOS Keychain Services through the keyring crate
---

Holds the two API tokens under the service dev.shrew.app, so that they are never written to the config file. Shrew reads them at start, writes them on sign-in, token renewal and when a Tempo token is saved, and deletes them on sign-out.
