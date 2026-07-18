---
id: example-memory-is-documentation-and-an-okf-parser-fixture
kind: memory
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
origin:
  version: memzoi/origin-v1
  origin_key: repository-record:example-memory-is-documentation-and-an-okf-parser-fixture
  route: repository_materialization
type: warning
lane: semantic
title: "Example memory is documentation and an OKF parser fixture"
description: "examples/example-memory.md is public-facing documentation and is also parsed by OKF profile tests and embedded test code. Changes must keep it valid under the current schema and update the related expectations."
timestamp: "2026-07-17T23:24:33.749369Z"
updated: "2026-07-17T23:24:33.749369Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: test
source_ref: "crates/memzoi-core/tests/okf_profile.rs; crates/memzoi-core/src/okf.rs; examples/example-memory.md"
proposal_id: prop_019f7264-86d9-7063-8e54-8e3922313a55
---

# Example memory is documentation and an OKF parser fixture

examples/example-memory.md is public-facing documentation and is also parsed by OKF profile tests and embedded test code. Changes must keep it valid under the current schema and update the related expectations.
