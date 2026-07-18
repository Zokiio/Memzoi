---
id: semantic/decisions/auth-session-validation
kind: memory
profile: memzoi
retention: {}
origin:
  origin_key: repository-record:compact-canonical-from-proposal
  route: repository_materialization
type: decision
lane: semantic
title: Protected routes must validate sessions server-side
description: Protected API routes must validate sessions server-side instead of trusting client auth state.
timestamp: 2026-07-06T00:00:00Z
status: active
visibility: repo
content_class: general_repo_knowledge
confidence: 1.0
scope: project
source: agent
source_ref: mem_2026_07_06_auth_001
applies_to:
  - src/auth/**
tags:
  - auth
  - middleware
  - security
---

# Protected routes must validate sessions server-side

Protected API routes must validate the session server-side. Do not trust client-side auth state for authorization decisions.
