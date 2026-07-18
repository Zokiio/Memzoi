---
id: mem_2026_07_06_auth_supersede
kind: proposal
profile: memzoi
retention: {}
origin:
  origin_key: repository-proposal:mem_2026_07_06_auth_supersede
  route: repository_proposal
type: decision
lane: semantic
title: Server sessions supersede client auth checks
description: Server-side session validation supersedes older guidance that trusted client auth state.
status: proposed
proposal:
  action: supersede
  proposed_by: agent
  proposed_at: 2026-07-06T00:45:00Z
  reason: Older auth guidance is now unsafe.
  confidence: medium
scope:
  kind: project
  paths:
    - src/auth/**
tags:
  - auth
  - security
timestamp: 2026-07-06T00:45:00Z
created_by: agent
sources:
  - path: src/auth/session.ts
supersedes:
  - semantic/decisions/auth-client-validation
sensitivity: repo-safe
content_class: general_repo_knowledge
---

# Server sessions supersede client auth checks

Protected routes should validate sessions server-side. This supersedes older guidance that treated client auth state as sufficient for authorization.

## Review notes

- Confirm the superseded record ID before applying.
