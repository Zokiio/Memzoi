---
id: expiry-removes-current-suppression-authority-but-preserves-permitted-history
kind: memory
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
origin:
  version: memzoi/origin-v1
  origin_key: repository-record:expiry-removes-current-suppression-authority-but-preserves-permitted-history
  route: repository_materialization
type: decision
lane: semantic
title: "Expiry removes current suppression authority but preserves permitted history"
description: "Target contract: duplicate suppression, contradiction suppression, candidate admission, ordinary recall, context packs, and prechecks share one current-assertion predicate. An expired record is outside that predicate and cannot reject a fresh candidate as an active duplicate, suppress a new current claim as a current contradiction, or remain an active member of a current conflict set. Expiry does not prove a competing member true; any surviving member must independently pass current eligibility and revalidation. Fresh attributable evidence matching expired content yields a renewal candidate or active successor, preserving lineage and validity history without reactivating or rewriting the old record. Replaying the same source event, plan, or operation remains idempotent regardless of expiry. Expired records may still support permitted lineage, renewal, operation idempotency, audit, recovery, explicit inspection, and time-qualified historical analysis. If retention or deletion policy prohibits body processing after expiry, only permitted identity, hash, tombstone, and audit metadata may remain available."
timestamp: "2026-07-17T23:13:24.938433Z"
updated: "2026-07-17T23:13:24.938433Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: issue
source_ref: "https://github.com/Zokiio/Memzoi/issues/50"
proposal_id: prop_019f7200-06cc-7061-b187-8065f3248164
---

# Expiry removes current suppression authority but preserves permitted history

Target contract: duplicate suppression, contradiction suppression, candidate admission, ordinary recall, context packs, and prechecks share one current-assertion predicate. An expired record is outside that predicate and cannot reject a fresh candidate as an active duplicate, suppress a new current claim as a current contradiction, or remain an active member of a current conflict set. Expiry does not prove a competing member true; any surviving member must independently pass current eligibility and revalidation. Fresh attributable evidence matching expired content yields a renewal candidate or active successor, preserving lineage and validity history without reactivating or rewriting the old record. Replaying the same source event, plan, or operation remains idempotent regardless of expiry. Expired records may still support permitted lineage, renewal, operation idempotency, audit, recovery, explicit inspection, and time-qualified historical analysis. If retention or deletion policy prohibits body processing after expiry, only permitted identity, hash, tombstone, and audit metadata may remain available.
