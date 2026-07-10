---
type: warning
lane: semantic
title: Saffron expiry sentinel boundary
timestamp: "2026-07-01T00:00:00Z"
updated: "2026-07-01T00:00:00Z"
status: active
scope: repo
visibility: repo
confidence: 1
source: eval
source_ref: fixture://expiry-boundary
expires: "2026-07-10T12:00:00Z"
applies_to:
  - crates/memzoi-core/src/expiry.rs
---

# Saffron expiry sentinel boundary

The boundary saffron expiry sentinel must be excluded exactly at its expiry instant.
