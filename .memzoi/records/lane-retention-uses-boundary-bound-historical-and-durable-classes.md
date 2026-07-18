---
type: decision
lane: semantic
title: "Lane retention uses boundary-bound, historical, and durable classes"
description: "Target contract: session records use boundary-bound retention and lose ordinary recall plus current duplicate/conflict authority at task or session closure or a finite safety cap; recall expiry does not itself physically delete retained data. Episodic records are historical evidence with a versioned automatic-recall horizon: recency may rank them within the horizon, while authorized explicit history, as-of, audit, recovery, lineage, and evidence queries remain possible afterward. Reading an episode never renews its horizon. Semantic and procedural records are durable and never expire, delete, supersede, renew, or lose authority from age or non-use alone; they change only through explicit expiry, authoritative supersession, an authorized lifecycle action, or reversible read-time safety suppression. Repository lifecycle actions remain Git-materialized and reviewed; an enabled private automatic-maintenance policy may authorize bounded private lifecycle actions. Lifecycle eligibility, automatic-recall eligibility, and physical retention are separate state dimensions."
timestamp: "2026-07-17T23:13:33.347532Z"
updated: "2026-07-17T23:13:33.347532Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: issue
source_ref: "https://github.com/Zokiio/Memzoi/issues/50"
proposal_id: prop_019f720b-8921-74c2-a585-a9c8d7e50681
---

# Lane retention uses boundary-bound, historical, and durable classes

Target contract: session records use boundary-bound retention and lose ordinary recall plus current duplicate/conflict authority at task or session closure or a finite safety cap; recall expiry does not itself physically delete retained data. Episodic records are historical evidence with a versioned automatic-recall horizon: recency may rank them within the horizon, while authorized explicit history, as-of, audit, recovery, lineage, and evidence queries remain possible afterward. Reading an episode never renews its horizon. Semantic and procedural records are durable and never expire, delete, supersede, renew, or lose authority from age or non-use alone; they change only through explicit expiry, authoritative supersession, an authorized lifecycle action, or reversible read-time safety suppression. Repository lifecycle actions remain Git-materialized and reviewed; an enabled private automatic-maintenance policy may authorize bounded private lifecycle actions. Lifecycle eligibility, automatic-recall eligibility, and physical retention are separate state dimensions.
