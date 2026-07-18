---
id: private-context-requires-separate-local-and-remote-release-operations
kind: memory
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
origin:
  version: memzoi/origin-v1
  origin_key: repository-record:private-context-requires-separate-local-and-remote-release-operations
  route: repository_materialization
type: decision
lane: semantic
title: "Private context requires separate local and remote release operations"
description: "Accepted target contract; not yet enforced by the current runtime. Implementation is tracked by #116–#118. A tool result containing private-memory plaintext is an egress event whenever its recipient may forward it to a remote processor. A trusted-local context operation may release raw private context only to an authenticated reader binding with a verified local-only processing boundary and matching private-read authority. include_local is a retrieval selector, never authority. Remote or mixed integrations use a separate approved-remote-context operation that requires both private-read and remote-disclosure grants. Memzoi performs retrieval, lifecycle and applicability filtering, content and sensitivity enforcement, local transformation, payload budgeting, final revalidation, and durable release auditing before returning the exact bounded representation. The processor identity and boundary derive from the trusted integration binding, not tool arguments. A shared release boundary covers every private-content route, including search, inspection, context and handoff packs, exports, diagnostics, maintenance output, CLI, MCP, and future provider APIs. Current build_context_pack is a plaintext release primitive rather than this enforcement model."
timestamp: "2026-07-17T23:12:56.016881Z"
updated: "2026-07-18T00:09:16Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: issue
source_ref: "https://github.com/Zokiio/Memzoi/issues/115"
proposal_id: prop_019f71d6-395c-7f73-ba99-12321d6830cf
---

# Private context requires separate local and remote release operations

Accepted target contract; not yet enforced by the current runtime. Implementation is tracked by #116–#118. A tool result containing private-memory plaintext is an egress event whenever its recipient may forward it to a remote processor. A trusted-local context operation may release raw private context only to an authenticated reader binding with a verified local-only processing boundary and matching private-read authority. include_local is a retrieval selector, never authority. Remote or mixed integrations use a separate approved-remote-context operation that requires both private-read and remote-disclosure grants. Memzoi performs retrieval, lifecycle and applicability filtering, content and sensitivity enforcement, local transformation, payload budgeting, final revalidation, and durable release auditing before returning the exact bounded representation. The processor identity and boundary derive from the trusted integration binding, not tool arguments. A shared release boundary covers every private-content route, including search, inspection, context and handoff packs, exports, diagnostics, maintenance output, CLI, MCP, and future provider APIs. Current build_context_pack is a plaintext release primitive rather than this enforcement model.
