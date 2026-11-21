---
type: C4 System
title: Tempo
description: Tempo Timesheets, an optional destination for logged time
status: stable
groma:
  id: tempo
  technology: Tempo REST API v4 at api.tempo.io over HTTPS, bearer token
---

When the user chooses Tempo as the worklog target, Shrew creates worklogs there instead of in Jira; Tempo mirrors them into Jira itself, so time is never posted to both. Shrew only creates worklogs in Tempo and reads nothing from it. The host is fixed in the app so that the Tempo token cannot be sent elsewhere.
