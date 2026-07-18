---
type: decision
lane: semantic
title: "Private continuity exports preserve canonical state without restoring authority"
description: "For #50, private continuity export is a direct owner-authorized, immutable point-in-time recovery/migration snapshot protected by mandatory versioned authenticated encryption. It contains only selected retained canonical private runtime records; canonical lifecycle, retention, applicability, lineage, dependency and conflict relations needed to interpret them; minimal non-secret provenance/policy identities; a manifest with snapshot/store/schema/completeness/selection/erasure-watermark data; and a minimal content-free erasure ledger. It excludes raw source events, prompts, transcripts, tool output, traces, content-bearing history, FTS/semantic indexes, embeddings, caches, generated context, deleted bodies, pre-redaction content, credentials, active grants/capabilities, and Git-governed repository OKF bodies. Restore authenticates into an isolated staging runtime, merges erasure barriers before loading records, rejects barred or older versions, validates current ownership/lifecycle policy, recomputes recall state, rebuilds derived indexes, verifies closure, then atomically activates. Newer destination erasure epochs and record versions always win. Restore never reactivates reader, inference, maintenance, disclosure, or integration authority. Export needs direct owner action; scheduled backups require a separate owner policy. An exported archive is an independent copy that later live deletion cannot modify, so export/restore results expose the erasure watermark and warn when newer erasure information may be missing."
timestamp: "2026-07-17T23:14:35.676448Z"
updated: "2026-07-17T23:14:35.676448Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: issue
source_ref: "https://github.com/Zokiio/Memzoi/issues/50"
proposal_id: prop_019f7246-a822-7891-a4f3-b3d38716a407
---

# Private continuity exports preserve canonical state without restoring authority

For #50, private continuity export is a direct owner-authorized, immutable point-in-time recovery/migration snapshot protected by mandatory versioned authenticated encryption. It contains only selected retained canonical private runtime records; canonical lifecycle, retention, applicability, lineage, dependency and conflict relations needed to interpret them; minimal non-secret provenance/policy identities; a manifest with snapshot/store/schema/completeness/selection/erasure-watermark data; and a minimal content-free erasure ledger. It excludes raw source events, prompts, transcripts, tool output, traces, content-bearing history, FTS/semantic indexes, embeddings, caches, generated context, deleted bodies, pre-redaction content, credentials, active grants/capabilities, and Git-governed repository OKF bodies. Restore authenticates into an isolated staging runtime, merges erasure barriers before loading records, rejects barred or older versions, validates current ownership/lifecycle policy, recomputes recall state, rebuilds derived indexes, verifies closure, then atomically activates. Newer destination erasure epochs and record versions always win. Restore never reactivates reader, inference, maintenance, disclosure, or integration authority. Export needs direct owner action; scheduled backups require a separate owner policy. An exported archive is an independent copy that later live deletion cannot modify, so export/restore results expose the erasure watermark and warn when newer erasure information may be missing.
