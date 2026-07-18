---
id: stream-runtime-event-exports-row-by-row
kind: memory
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
origin:
  version: memzoi/origin-v1
  origin_key: repository-record:stream-runtime-event-exports-row-by-row
  route: repository_materialization
type: decision
lane: semantic
title: "Stream runtime event exports row by row"
description: "Runtime event logs are append-only and can grow without bound. Event-export consumers visit and emit rows incrementally rather than materializing the full log before output. In-memory event collection is test-only."
timestamp: "2026-07-17T23:25:37.426231Z"
updated: "2026-07-17T23:25:37.426231Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: code
source_ref: "crates/memzoi-core/src/events.rs; crates/memzoi-cli/src/commands.rs"
proposal_id: prop_019f7264-dac2-7041-a469-5484580b72bb
---

# Stream runtime event exports row by row

Runtime event logs are append-only and can grow without bound. Event-export consumers visit and emit rows incrementally rather than materializing the full log before output. In-memory event collection is test-only.
