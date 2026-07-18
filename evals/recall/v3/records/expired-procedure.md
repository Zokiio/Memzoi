---
id: expired-procedure
kind: memory
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
  explicit_expires_at: "2026-07-01T00:00:00Z"
origin:
  version: memzoi/origin-v1
  origin_key: eval-record:expired-procedure
  route: repository_materialization
type: procedure
lane: procedural
title: Temporary migration window
timestamp: "2026-06-01T00:00:00Z"
updated: "2026-06-01T00:00:00Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: eval
source_ref: fixture://expired-procedure
---

# Temporary migration window

The temporary migration window allowed legacy index promotion before July and is now expired.
