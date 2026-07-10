---
type: warning
lane: semantic
title: Magenta precheck sentinel unrelated warning
timestamp: "2026-07-01T00:00:00Z"
updated: "2026-07-01T00:00:00Z"
status: active
scope: repo
visibility: repo
confidence: 1
source: eval
source_ref: fixture://precheck-path-unrelated
applies_to:
  - crates/memzoi-core/src/search.rs
---

# Magenta precheck sentinel unrelated warning

Changing the unrelated ranking order can destabilize a separate result set.
