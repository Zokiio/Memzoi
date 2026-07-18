---
id: lifecycle-active
kind: memory
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
origin:
  version: memzoi/origin-v1
  origin_key: eval-record:lifecycle-active
  route: repository_materialization
type: warning
lane: semantic
title: Crimson lifecycle sentinel active
timestamp: "2026-07-01T00:00:00Z"
updated: "2026-07-02T00:00:00Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: eval
source_ref: fixture://lifecycle-active
applies_to:
  - crates/memzoi-core/src/service.rs
---

# Crimson lifecycle sentinel active

The active crimson lifecycle sentinel remains eligible for normal recall.
