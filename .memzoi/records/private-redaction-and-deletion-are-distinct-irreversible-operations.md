---
id: private-redaction-and-deletion-are-distinct-irreversible-operations
kind: memory
profile: memzoi
retention: {}
origin:
  origin_key: repository-record:private-redaction-and-deletion-are-distinct-irreversible-operations
  route: repository_materialization
type: decision
lane: semantic
title: "Private redaction and deletion are distinct irreversible operations"
description: "For #50, private correction/supersession, quarantine, redaction, and deletion have distinct semantics. Quarantine is reversible recall suppression retaining content. Exact owner-authorized redaction irreversibly removes selected content, content-bearing history/evidence, and all known live derivatives while preserving only a valid non-redacted remainder; that remainder must be re-admitted for classification, scope, lane, applicability, conflict, reader, and disclosure policy. An empty, misleading, unsupported, or structurally invalid remainder blocks with redaction_requires_delete; redaction never implies deletion authority. Exact owner-authorized deletion irreversibly removes a whole record and its content-bearing metadata from canonical runtime, history, evidence, indexes, embeddings, caches, artifacts, exports, queues, and active replicas. It leaves no usable memory and cannot be recreated by replay of the exact source event, though genuinely new evidence can form a new record unless a separate do_not_remember rule exists. Redaction/deletion are never authorized by inference, maintenance opt-in, age, repository configuration, or caller input. Execution first recall-blocks the target, validates the plan/snapshot/grant, removes derivatives, then stores only minimal non-content audit/tombstone metadata permitted for sync/audit/idempotency. Results must distinguish operational erasure from replica cleanup, backup retirement, and verified storage sanitization; backup restore must replay erasure ledger before recall. Do not retain removed content in audit/history, and never overclaim physical sanitization."
timestamp: "2026-07-17T23:14:23.6088Z"
updated: "2026-07-17T23:14:23.6088Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: issue
source_ref: "https://github.com/Zokiio/Memzoi/issues/50"
proposal_id: prop_019f723a-9509-75a3-941a-a100f2e3a37a
---

# Private redaction and deletion are distinct irreversible operations

For #50, private correction/supersession, quarantine, redaction, and deletion have distinct semantics. Quarantine is reversible recall suppression retaining content. Exact owner-authorized redaction irreversibly removes selected content, content-bearing history/evidence, and all known live derivatives while preserving only a valid non-redacted remainder; that remainder must be re-admitted for classification, scope, lane, applicability, conflict, reader, and disclosure policy. An empty, misleading, unsupported, or structurally invalid remainder blocks with redaction_requires_delete; redaction never implies deletion authority. Exact owner-authorized deletion irreversibly removes a whole record and its content-bearing metadata from canonical runtime, history, evidence, indexes, embeddings, caches, artifacts, exports, queues, and active replicas. It leaves no usable memory and cannot be recreated by replay of the exact source event, though genuinely new evidence can form a new record unless a separate do_not_remember rule exists. Redaction/deletion are never authorized by inference, maintenance opt-in, age, repository configuration, or caller input. Execution first recall-blocks the target, validates the plan/snapshot/grant, removes derivatives, then stores only minimal non-content audit/tombstone metadata permitted for sync/audit/idempotency. Results must distinguish operational erasure from replica cleanup, backup retirement, and verified storage sanitization; backup restore must replay erasure ledger before recall. Do not retain removed content in audit/history, and never overclaim physical sanitization.
