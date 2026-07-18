---
type: decision
lane: semantic
title: "Personal private memory uses separate cross-integration read grants"
description: "Accepted target contract; not yet enforced by the current runtime. Implementation is tracked by #116. Personal private memory belongs to the owner principal rather than the creating agent. Creation provenance remains auditable but grants no read authority. Another integration may retrieve it only through an authenticated same-owner reader binding and a current, separately owner-approved private-read grant bounded by destination, scope, lane, content class, sensitivity, purpose, and expiry. Repository configuration and record provenance cannot activate or widen this authority."
timestamp: "2026-07-17T23:14:54.118634Z"
updated: "2026-07-18T00:09:16Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: issue
source_ref: "https://github.com/Zokiio/Memzoi/issues/115"
proposal_id: prop_019f7258-ee64-7432-8298-df1e8f87a5d4
---

# Personal private memory uses separate cross-integration read grants

Accepted target contract; not yet enforced by the current runtime. Implementation is tracked by #116. Personal private memory belongs to the owner principal rather than the creating agent. Creation provenance remains auditable but grants no read authority. Another integration may retrieve it only through an authenticated same-owner reader binding and a current, separately owner-approved private-read grant bounded by destination, scope, lane, content class, sensitivity, purpose, and expiry. Repository configuration and record provenance cannot activate or widen this authority.
