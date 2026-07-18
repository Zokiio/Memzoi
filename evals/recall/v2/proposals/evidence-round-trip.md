---
id: mem_eval_evidence_round_trip
kind: proposal
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
origin:
  version: memzoi/origin-v1
  origin_key: eval:proposal-evidence-round-trip
  route: repository_proposal
type: decision
lane: semantic
title: Proposal evidence round trip
description: The opaline proposal sentinel preserves evidence separately from applicability and review lineage.
status: proposed
proposal:
  action: create
  proposed_by: eval
  proposed_at: "2026-07-01T00:00:00Z"
  reason: Verify evidence provenance across proposal apply, rebuild, recall, and export.
scope:
  kind: repo
  paths:
    - crates/memzoi-core/src/recall_eval.rs
tags:
  - eval
  - provenance
timestamp: "2026-07-01T00:00:00Z"
created_by: eval
sources:
  - path: docs/rfcs/0001-evidence-backed-capture.md
supersedes: []
sensitivity: repo-safe
content_class: general_repo_knowledge
---

# Proposal evidence round trip

The opaline proposal evidence sentinel must preserve its original evidence source while the approving proposal remains separate lineage.
