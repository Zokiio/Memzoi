---
type: risk
lane: semantic
title: Magenta precheck sentinel risk
timestamp: "2026-07-01T00:00:00Z"
updated: "2026-07-01T00:00:00Z"
status: active
scope: repo
visibility: repo
confidence: 1
source: eval
source_ref: fixture://precheck-path-target
applies_to:
  - crates/memzoi-core/src/precheck.rs
---

# Magenta precheck sentinel risk

Changing the settlement order can silently corrupt evaluated totals.
