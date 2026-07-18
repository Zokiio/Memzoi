---
id: expiry-boundary
kind: memory
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
  explicit_expires_at: "2026-07-10T12:00:00Z"
origin:
  version: memzoi/origin-v1
  origin_key: eval-record:expiry-boundary
  route: repository_materialization
type: warning
lane: semantic
title: Saffron expiry sentinel boundary
timestamp: "2026-07-01T00:00:00Z"
updated: "2026-07-01T00:00:00Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: eval
source_ref: fixture://expiry-boundary
applies_to:
  - crates/memzoi-core/src/expiry.rs
---

# Saffron expiry sentinel boundary

The boundary saffron expiry sentinel must be excluded exactly at its expiry instant.
