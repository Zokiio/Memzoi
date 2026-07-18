---
id: maintenance-plans-separate-evidence-from-execution-authority
kind: memory
profile: memzoi
retention: {}
origin:
  origin_key: repository-record:maintenance-plans-separate-evidence-from-execution-authority
  route: repository_materialization
type: decision
lane: semantic
title: "Maintenance plans separate evidence from execution authority"
description: "For #50, use one immutable, snapshot-bound maintenance-plan format across repository and private runtime memory. Planning is mutation-free and captures plan/schema identity, evaluated_at and not_after, scope, target versions/hashes, comparison-set digests, detector and policy versions, evidence, actions, and stale preconditions. Plans record no transferable authority; every grant and precondition is revalidated at execution. Repository materialization, private derived-state application, and owner-authorized private mutation are separate atomic execution groups with no claimed cross-storage transaction. Repository execution can only materialize unstaged Git-reviewable OKF create/supersede/tombstone changes; it never stages, commits, pushes, merges, or accepts. Private maintenance is report-only by default. An explicit maintenance grant may only persist reversible, rebuildable, non-semantic derived recall state: initially high-confidence unresolved-conflict suppression. Expiry is an always-on read invariant derived from accepted record/lane policy, regardless of maintenance opt-in; overlays are only cache/audit state. Renewal, promotion, supersession, merge/consolidation, contradiction winner selection, redaction, deletion/purge, retention extension/pin, and classification/destination changes require exact owner authorization for the action or direct typed owner event. Expiry does not by itself authorize physical deletion."
timestamp: "2026-07-17T23:14:03.288013Z"
updated: "2026-07-17T23:14:03.288013Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: issue
source_ref: "https://github.com/Zokiio/Memzoi/issues/50"
proposal_id: prop_019f7232-ddd5-75c2-aafb-fe04762b9a5e
---

# Maintenance plans separate evidence from execution authority

For #50, use one immutable, snapshot-bound maintenance-plan format across repository and private runtime memory. Planning is mutation-free and captures plan/schema identity, evaluated_at and not_after, scope, target versions/hashes, comparison-set digests, detector and policy versions, evidence, actions, and stale preconditions. Plans record no transferable authority; every grant and precondition is revalidated at execution. Repository materialization, private derived-state application, and owner-authorized private mutation are separate atomic execution groups with no claimed cross-storage transaction. Repository execution can only materialize unstaged Git-reviewable OKF create/supersede/tombstone changes; it never stages, commits, pushes, merges, or accepts. Private maintenance is report-only by default. An explicit maintenance grant may only persist reversible, rebuildable, non-semantic derived recall state: initially high-confidence unresolved-conflict suppression. Expiry is an always-on read invariant derived from accepted record/lane policy, regardless of maintenance opt-in; overlays are only cache/audit state. Renewal, promotion, supersession, merge/consolidation, contradiction winner selection, redaction, deletion/purge, retention extension/pin, and classification/destination changes require exact owner authorization for the action or direct typed owner event. Expiry does not by itself authorize physical deletion.
