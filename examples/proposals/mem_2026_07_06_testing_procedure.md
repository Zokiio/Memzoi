---
id: mem_2026_07_06_testing_procedure
kind: proposal
version: okf/v0.1
profile: memzoi/v0
type: procedure
lane: procedural
title: Run focused tests before full workspace checks
description: Start with focused tests for the touched crate, then run full workspace checks before opening a PR.
status: proposed
proposal:
  action: create
  proposed_by: agent
  proposed_at: 2026-07-06T00:30:00Z
  reason: Captures the repo's preferred validation workflow.
  confidence: high
scope:
  kind: repo
  paths:
    - crates/**
tags:
  - testing
  - workflow
timestamp: 2026-07-06T00:30:00Z
created_by: agent
sources:
  - path: README.md
supersedes: []
sensitivity: repo-safe
---

# Run focused tests before full workspace checks

When changing one crate, run the smallest relevant test target first. Before opening a PR, run formatting, workspace tests, clippy, and docs checks when docs changed.

## Review notes

- Confirm the exact command list against current CI before applying.
