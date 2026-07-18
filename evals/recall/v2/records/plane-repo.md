---
id: plane-repo
kind: memory
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
origin:
  version: memzoi/origin-v1
  origin_key: eval-record:plane-repo
  route: repository_materialization
type: fact
lane: semantic
title: Onyx memory plane sentinel
timestamp: "2026-07-01T00:00:00Z"
updated: "2026-07-01T00:00:00Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: eval
source_ref: fixture://plane-repo
applies_to:
  - docs/evaluation.md
---

# Onyx memory plane sentinel

The onyx memory plane sentinel is canonical repository memory with Git provenance.
