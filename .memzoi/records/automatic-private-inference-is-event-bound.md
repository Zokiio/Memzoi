---
type: decision
lane: semantic
title: "Automatic private inference is event-bound"
description: "Automatic private inference applies only to an explicit typed lifecycle event from an authorized integration. Each event has a known emitter, event type, scope, bounded evidence, policy, and identity or idempotency key. It never permits ambient scanning of chat, hidden state, shell history, editor activity, repository files, or arbitrary tool output."
timestamp: "2026-07-17T23:12:06.830197Z"
updated: "2026-07-17T23:12:06.830197Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: issue
source_ref: "https://github.com/Zokiio/Memzoi/issues/50"
proposal_id: prop_019f711c-6ce5-7072-be4b-3efae8243e1f
---

# Automatic private inference is event-bound

Automatic private inference applies only to an explicit typed lifecycle event from an authorized integration. Each event has a known emitter, event type, scope, bounded evidence, policy, and identity or idempotency key. It never permits ambient scanning of chat, hidden state, shell history, editor activity, repository files, or arbitrary tool output.
