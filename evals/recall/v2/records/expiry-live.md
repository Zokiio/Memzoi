---
id: expiry-live
kind: memory
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
  explicit_expires_at: "2026-07-10T12:00:01Z"
origin:
  version: memzoi/origin-v1
  origin_key: eval-record:expiry-live
  route: repository_materialization
type: warning
lane: semantic
title: Saffron expiry sentinel live
timestamp: "2026-07-01T00:00:00Z"
updated: "2026-07-01T00:00:00Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: eval
source_ref: fixture://expiry-live
applies_to:
  - crates/memzoi-core/src/expiry.rs
---

# Saffron expiry sentinel live

The live saffron expiry sentinel remains readable one second before its expiry.
