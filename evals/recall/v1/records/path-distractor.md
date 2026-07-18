---
id: path-distractor
kind: memory
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
origin:
  version: memzoi/origin-v1
  origin_key: eval-record:path-distractor
  route: repository_materialization
type: decision
lane: semantic
title: Cerulean path sentinel
timestamp: "2026-07-01T00:00:00Z"
updated: "2026-07-01T00:00:00Z"
status: active
scope: repo
visibility: repo
confidence: 1
source: eval
source_ref: fixture://path-distractor
applies_to:
  - apps/web/src/search.ts
---

# Cerulean path sentinel

This cerulean path sentinel belongs only to an unrelated web search path.
