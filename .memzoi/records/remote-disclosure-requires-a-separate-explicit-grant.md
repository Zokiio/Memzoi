---
type: decision
lane: semantic
title: "Remote disclosure requires a separate explicit grant"
description: "Accepted target contract; not yet enforced by the current runtime. Implementation is tracked by #117. A private-read grant authorizes local retrieval only. Sending recalled private memory beyond the local trust boundary to a remote or mixed processor requires a separate, deny-by-default owner grant bound to the reader, processor, purpose, selection, representation, sensitivity, payload budget, expiry, and revocation state. Memzoi must minimize, transform, revalidate, and audit the exact released representation before egress; the caller cannot receive a raw private superset and filter it remotely."
timestamp: "2026-07-17T23:15:16.107296Z"
updated: "2026-07-18T00:09:16Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: issue
source_ref: "https://github.com/Zokiio/Memzoi/issues/115"
proposal_id: prop_019f7259-06b1-7cd3-abfd-24403c9c73bc
---

# Remote disclosure requires a separate explicit grant

Accepted target contract; not yet enforced by the current runtime. Implementation is tracked by #117. A private-read grant authorizes local retrieval only. Sending recalled private memory beyond the local trust boundary to a remote or mixed processor requires a separate, deny-by-default owner grant bound to the reader, processor, purpose, selection, representation, sensitivity, payload budget, expiry, and revocation state. Memzoi must minimize, transform, revalidate, and audit the exact released representation before egress; the caller cannot receive a raw private superset and filter it remotely.
