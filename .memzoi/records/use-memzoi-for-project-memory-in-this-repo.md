---
id: use-memzoi-for-project-memory-in-this-repo
kind: memory
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
origin:
  version: memzoi/origin-v1
  origin_key: repository-record:use-memzoi-for-project-memory-in-this-repo
  route: repository_materialization
type: decision
lane: semantic
title: "Use Memzoi for project memory in this repo"
description: "This repo is initialized with Memzoi. Durable project memory should be stored as reviewed OKF-profile Markdown records under .memzoi/records/. Local runtime state lives outside the repo under the Memzoi home directory; rebuild it from canonical records with memzoi rebuild."
timestamp: "2026-07-05T11:31:45.871789Z"
updated: "2026-07-05T11:31:45.871789Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: cli
source_ref: prop_019f320c-6385-7653-a5d0-be04616f2235
---

# Use Memzoi for project memory in this repo

This repo is initialized with Memzoi. Durable project memory should be stored as reviewed OKF-profile Markdown records under .memzoi/records/. Local runtime state lives outside the repo under the Memzoi home directory; rebuild it from canonical records with memzoi rebuild.
