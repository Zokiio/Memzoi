---
id: inventory-duplicate
kind: memory
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
origin:
  version: memzoi/origin-v1
  origin_key: eval-record:inventory-duplicate
  route: repository_materialization
type: fact
lane: semantic
title: Existing inventory memory
timestamp: "2026-07-10T00:00:00Z"
updated: "2026-07-10T00:00:00Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: eval
source_ref: fixture://capture-inventory-duplicate
applies_to:
  - notes/inventory.md
---

# Existing inventory memory

An inventory-backed duplicate remains suppressed.
