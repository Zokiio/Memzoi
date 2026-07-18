---
type: decision
lane: semantic
title: "Versioned lane-retention defaults use bounded recall horizons"
description: "For #50, adopt versioned initial defaults that govern lifecycle and ordinary automatic-recall eligibility, not physical deletion. Session records expire at the earliest accepted terminal boundary, a 24-hour continuation lease, or a seven-day absolute cap from started_at. A continuation must be typed, authorized, attributable to the same active session, and accepted before expiry; it sets the lease to at most 24 hours from the continuation event and never reopens a closed session. Work after closure/cap creates a successor session with handoff provenance. Episodic records are ordinarily auto-recall eligible for 30 days from occurred_at; an owner-authorized retention policy may extend no later than 90 days from occurrence. Retrieval, ranking, citation, indexing, and checkpoints do not slide either boundary; thereafter the record is query-only subject to authorization. Semantic and procedural records have no age-based TTL; only explicit expiry, authoritative supersession, invalid applicability/dependency, accepted conflict/safety state, authorized lifecycle action, or owner deletion changes their eligibility. These defaults must be evaluated against stale recall, missed context, privacy, and review-burden metrics before revision."
timestamp: "2026-07-17T23:13:52.376483Z"
updated: "2026-07-17T23:13:52.376483Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: issue
source_ref: "https://github.com/Zokiio/Memzoi/issues/50"
proposal_id: prop_019f722a-09f8-7523-adbd-07861226eecc
---

# Versioned lane-retention defaults use bounded recall horizons

For #50, adopt versioned initial defaults that govern lifecycle and ordinary automatic-recall eligibility, not physical deletion. Session records expire at the earliest accepted terminal boundary, a 24-hour continuation lease, or a seven-day absolute cap from started_at. A continuation must be typed, authorized, attributable to the same active session, and accepted before expiry; it sets the lease to at most 24 hours from the continuation event and never reopens a closed session. Work after closure/cap creates a successor session with handoff provenance. Episodic records are ordinarily auto-recall eligible for 30 days from occurred_at; an owner-authorized retention policy may extend no later than 90 days from occurrence. Retrieval, ranking, citation, indexing, and checkpoints do not slide either boundary; thereafter the record is query-only subject to authorization. Semantic and procedural records have no age-based TTL; only explicit expiry, authoritative supersession, invalid applicability/dependency, accepted conflict/safety state, authorized lifecycle action, or owner deletion changes their eligibility. These defaults must be evaluated against stale recall, missed context, privacy, and review-burden metrics before revision.
