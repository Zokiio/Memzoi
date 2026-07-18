---
id: example-memory
kind: memory
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
origin:
  version: memzoi/origin-v1
  origin_key: repository-record:example-memory
  route: repository_materialization
type: preference
lane: semantic
title: Swedish-first UI copy
description: User-facing UI and i18n text should be Swedish-first.
timestamp: 2026-07-04T00:00:00Z
status: active
visibility: team
content_class: general_repo_knowledge
confidence: 1.0
scope: repo
source: human
source_ref: memories/repo/frontend/swedish-first
applies_to:
  - apps/web/**
tags:
  - frontend
  - i18n
---

# Swedish-first UI copy

User-facing UI and i18n text should be Swedish-first.

Source code, identifiers, comments, and implementation text should remain English.
