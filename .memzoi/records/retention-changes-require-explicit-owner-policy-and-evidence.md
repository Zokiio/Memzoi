---
type: decision
lane: semantic
title: "Retention changes require explicit owner policy and evidence"
description: "Target contract: an integration may request but never unilaterally apply retention changes. Automatic application requires an authenticated binding, an allowed typed event, fresh bounded evidence, a user-owned versioned maintenance policy, an explicit action grant, lane-specific limits, and an atomic audit. Extension, pinning, and renewal are distinct actions over separate automatic-recall, validity, and physical-retention clocks; changing one does not change another. A policy may authorize bounded extension or evidence-backed renewal. Pinning is deny-by-default, requires direct owner intent or a narrow pin grant with reason and review boundary, and may never keep a session indefinitely active. Recall, ranking, citations, access frequency, model confidence, and integration liveness are never renewal evidence. Expired records renew only through fresh evidence and a new generation or successor; replay of the source event remains idempotent. Repository configuration cannot activate or widen private retention authority, and repository lifecycle stays Git-reviewed."
timestamp: "2026-07-17T23:13:41.521904Z"
updated: "2026-07-17T23:13:41.521904Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: issue
source_ref: "https://github.com/Zokiio/Memzoi/issues/50"
proposal_id: prop_019f7216-0146-7400-8165-d78add5da1c3
---

# Retention changes require explicit owner policy and evidence

Target contract: an integration may request but never unilaterally apply retention changes. Automatic application requires an authenticated binding, an allowed typed event, fresh bounded evidence, a user-owned versioned maintenance policy, an explicit action grant, lane-specific limits, and an atomic audit. Extension, pinning, and renewal are distinct actions over separate automatic-recall, validity, and physical-retention clocks; changing one does not change another. A policy may authorize bounded extension or evidence-backed renewal. Pinning is deny-by-default, requires direct owner intent or a narrow pin grant with reason and review boundary, and may never keep a session indefinitely active. Recall, ranking, citations, access frequency, model confidence, and integration liveness are never renewal evidence. Expired records renew only through fresh evidence and a new generation or successor; replay of the source event remains idempotent. Repository configuration cannot activate or widen private retention authority, and repository lifecycle stays Git-reviewed.
