---
id: private-reader-identity-requires-an-authenticated-integration-binding
kind: memory
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
origin:
  version: memzoi/origin-v1
  origin_key: repository-record:private-reader-identity-requires-an-authenticated-integration-binding
  route: repository_materialization
type: decision
lane: semantic
title: "Private reader identity requires an authenticated integration binding"
description: "Accepted target contract; not yet enforced by the current runtime. Implementation is tracked by #116. Reader identity for private memory is anchored in a user-approved integration binding proved at the connection boundary. Claimed agent or model names, MCP client metadata, tool arguments, process names, stdio or localhost transport, record origin, repository configuration, and unauthenticated environment labels are diagnostic only and never grant private-read authority. Bindings and grants are locally protected, inspectable, revocable, rotatable, owner-scoped, and fail closed."
timestamp: "2026-07-17T23:15:04.256264Z"
updated: "2026-07-18T00:09:16Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: issue
source_ref: "https://github.com/Zokiio/Memzoi/issues/115"
proposal_id: prop_019f7258-fb55-7fb3-b426-543b050868b6
---

# Private reader identity requires an authenticated integration binding

Accepted target contract; not yet enforced by the current runtime. Implementation is tracked by #116. Reader identity for private memory is anchored in a user-approved integration binding proved at the connection boundary. Claimed agent or model names, MCP client metadata, tool arguments, process names, stdio or localhost transport, record origin, repository configuration, and unauthenticated environment labels are diagnostic only and never grant private-read authority. Bindings and grants are locally protected, inspectable, revocable, rotatable, owner-scoped, and fail closed.
