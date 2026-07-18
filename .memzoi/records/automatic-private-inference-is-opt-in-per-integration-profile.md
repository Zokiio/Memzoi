---
id: automatic-private-inference-is-opt-in-per-integration-profile
kind: memory
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
origin:
  version: memzoi/origin-v1
  origin_key: repository-record:automatic-private-inference-is-opt-in-per-integration-profile
  route: repository_materialization
type: decision
lane: semantic
title: "Automatic private inference is opt-in per integration profile"
description: "Automatic private inference is opt-in per integration and versioned local inference profile, optionally narrowed to a repo or workspace. It is distinct from automatic recall, explicit private writes, automatic private maintenance, and repository materialization. Connecting an MCP or accepting a repo recommendation cannot activate or widen it; material expansion requires renewed opt-in."
timestamp: "2026-07-17T23:12:18.56111Z"
updated: "2026-07-17T23:12:18.56111Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: issue
source_ref: "https://github.com/Zokiio/Memzoi/issues/50"
proposal_id: prop_019f711c-7ec7-7252-abc9-a47203da0b86
---

# Automatic private inference is opt-in per integration profile

Automatic private inference is opt-in per integration and versioned local inference profile, optionally narrowed to a repo or workspace. It is distinct from automatic recall, explicit private writes, automatic private maintenance, and repository materialization. Connecting an MCP or accepting a repo recommendation cannot activate or widen it; material expansion requires renewed opt-in.
