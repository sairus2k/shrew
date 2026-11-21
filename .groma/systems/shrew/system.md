---
type: C4 System
title: Shrew
status: stable
groma:
  id: shrew
description: Local-first macOS desktop client for Jira Cloud
---

Gives a developer fast access to the issues of the Jira projects they watch: full-text search, issue details with comments and worklogs, status transitions, assignee changes, comments, a running timer and manual time logging. It keeps a local copy of those issues and an outbox of the changes made here, so reads and writes are instant and work offline; a background sync brings the copy in step with Jira and delivers the outbox to Jira or, for worklogs, to Tempo.

The system is one desktop application. Its data lives on the user's Mac: a SQLite file and a config file under `~/Library/Application Support/dev.shrew.app`, and the API tokens in the macOS Keychain.
