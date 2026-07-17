---
type: decision
lane: semantic
title: "Private context requires separate local and remote release operations"
description: "Target contract: a tool result containing private-memory plaintext is an egress event whenever its recipient may forward it to a remote processor. A trusted-local context operation may release raw private context only to an authenticated reader binding with a verified local-only processing boundary and matching private-read authority. include_local is a retrieval selector, never authority. Remote or mixed integrations use a separate approved-remote-context operation that requires both private-read and remote-disclosure grants. Memzoi performs retrieval, lifecycle and applicability filtering, content and sensitivity enforcement, local transformation, payload budgeting, final revalidation, and durable release auditing before returning the exact bounded representation. The processor identity and boundary derive from the trusted integration binding, not tool arguments. A shared release boundary covers every private-content route, including search, inspection, context and handoff packs, exports, diagnostics, maintenance output, CLI, MCP, and future provider APIs. This is a target contract; current build_context_pack is a plaintext release primitive rather than this enforcement model."
timestamp: "2026-07-17T23:12:56.016881Z"
updated: "2026-07-17T23:12:56.016881Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: issue
source_ref: "https://github.com/Zokiio/Memzoi/issues/57"
proposal_id: prop_019f71d6-395c-7f73-ba99-12321d6830cf
---

# Private context requires separate local and remote release operations

Target contract: a tool result containing private-memory plaintext is an egress event whenever its recipient may forward it to a remote processor. A trusted-local context operation may release raw private context only to an authenticated reader binding with a verified local-only processing boundary and matching private-read authority. include_local is a retrieval selector, never authority. Remote or mixed integrations use a separate approved-remote-context operation that requires both private-read and remote-disclosure grants. Memzoi performs retrieval, lifecycle and applicability filtering, content and sensitivity enforcement, local transformation, payload budgeting, final revalidation, and durable release auditing before returning the exact bounded representation. The processor identity and boundary derive from the trusted integration binding, not tool arguments. A shared release boundary covers every private-content route, including search, inspection, context and handoff packs, exports, diagnostics, maintenance output, CLI, MCP, and future provider APIs. This is a target contract; current build_context_pack is a plaintext release primitive rather than this enforcement model.
