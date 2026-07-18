---
id: mem_2026_07_06_auth_tombstone
kind: proposal
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
origin:
  version: memzoi/origin-v1
  origin_key: repository-proposal:mem_2026_07_06_auth_tombstone
  route: repository_proposal
type: decision
lane: semantic
title: Tombstone obsolete client auth guidance
description: Obsolete guidance that trusts client auth state should be tombstoned.
status: proposed
proposal:
  action: tombstone
  proposed_by: agent
  proposed_at: 2026-07-06T01:00:00Z
  reason: The guidance conflicts with current server-side session validation policy.
  confidence: medium
  target: semantic/decisions/auth-client-validation
scope:
  kind: project
  paths:
    - src/auth/**
tags:
  - auth
  - security
timestamp: 2026-07-06T01:00:00Z
created_by: agent
sources:
  - path: src/auth/session.ts
supersedes: []
sensitivity: repo-safe
content_class: general_repo_knowledge
---

# Tombstone obsolete client auth guidance

The target memory should be marked tombstoned because it recommends trusting client auth state for protected routes.

## Review notes

- Confirm that no current code path still depends on the old guidance.
