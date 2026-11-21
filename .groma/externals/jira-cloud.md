---
type: C4 System
title: Jira Cloud
description: The Atlassian Jira site that owns the issues
status: stable
groma:
  id: jira-cloud
  technology: REST API v3 over HTTPS, basic auth with an API token
---

The source of truth for projects, issues, comments, transitions, assignable users and worklogs. Shrew reads them into its local cache and sends back the changes the user makes: status transitions, assignee changes, comments and, unless Tempo is the worklog target, worklogs. A 401 answer pauses the sync until the user enters a new API token.
