---
id: lifecycle-superseded
kind: memory
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
origin:
  version: memzoi/origin-v1
  origin_key: eval-record:lifecycle-superseded
  route: repository_materialization
type: warning
lane: semantic
title: Crimson lifecycle sentinel superseded
timestamp: "2026-06-01T00:00:00Z"
updated: "2026-07-02T00:00:00Z"
status: superseded
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: eval
source_ref: fixture://lifecycle-superseded
applies_to:
  - crates/memzoi-core/src/service.rs
---

# Crimson lifecycle sentinel superseded

The superseded crimson lifecycle sentinel must not appear in normal recall.
