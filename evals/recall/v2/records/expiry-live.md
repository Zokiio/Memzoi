---
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
expires: "2026-07-10T12:00:01Z"
applies_to:
  - crates/memzoi-core/src/expiry.rs
---

# Saffron expiry sentinel live

The live saffron expiry sentinel remains readable one second before its expiry.
