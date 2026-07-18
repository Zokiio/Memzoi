---
id: precheck-path-fact
kind: memory
profile: memzoi
retention: {}
origin:
  origin_key: eval-record:precheck-path-fact
  route: repository_materialization
type: fact
lane: semantic
title: Magenta precheck sentinel same-path fact
timestamp: "2026-07-01T00:00:00Z"
updated: "2026-07-01T00:00:00Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: eval
source_ref: fixture://precheck-path-fact
applies_to:
  - crates/memzoi-core/src/precheck.rs
---

# Magenta precheck sentinel same-path fact

The same path contains an informational fact that is not governance memory.
