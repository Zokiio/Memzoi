---
id: tombstoned-procedure
kind: memory
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
origin:
  version: memzoi/origin-v1
  origin_key: eval-record:tombstoned-procedure
  route: repository_materialization
type: procedure
lane: procedural
title: Deleted direct deployment
timestamp: "2026-06-01T00:00:00Z"
updated: "2026-07-01T00:00:00Z"
status: tombstoned
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: eval
source_ref: fixture://tombstoned-procedure
---

# Deleted direct deployment

The deleted procedure replaced live release files directly without an atomic pointer.
