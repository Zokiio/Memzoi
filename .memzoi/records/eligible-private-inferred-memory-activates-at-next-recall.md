---
type: decision
lane: semantic
title: "Eligible private inferred memory activates at next recall"
description: "An authorized typed event may run inference but does not guarantee admission. A candidate that passes profile admission and write-time reconciliation is atomically persisted as an active private local runtime record and becomes eligible at the next independent recall boundary; it never affects the emitting operation retroactively. The default activation mode is apply_if_eligible with retrospective review. Unsafe or uncertain candidates are blocked, quarantined, or retained only as explicitly configured non-recallable review artifacts. Inferred memory remains lower authority than current explicit instructions and verified current state, and it never grants repository materialization authority."
timestamp: "2026-07-17T23:12:45.933266Z"
updated: "2026-07-17T23:12:45.933266Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: issue
source_ref: "https://github.com/Zokiio/Memzoi/issues/50"
proposal_id: prop_019f719d-8cf5-7821-b9fd-7ec6ed07d67f
---

# Eligible private inferred memory activates at next recall

An authorized typed event may run inference but does not guarantee admission. A candidate that passes profile admission and write-time reconciliation is atomically persisted as an active private local runtime record and becomes eligible at the next independent recall boundary; it never affects the emitting operation retroactively. The default activation mode is apply_if_eligible with retrospective review. Unsafe or uncertain candidates are blocked, quarantined, or retained only as explicitly configured non-recallable review artifacts. Inferred memory remains lower authority than current explicit instructions and verified current state, and it never grants repository materialization authority.
