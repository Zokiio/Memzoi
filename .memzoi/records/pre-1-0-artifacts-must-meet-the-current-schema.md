---
id: pre-1-0-artifacts-must-meet-the-current-schema
kind: memory
profile: memzoi
retention: {}
origin:
  origin_key: repository-record:pre-1-0-artifacts-must-meet-the-current-schema
  route: repository_materialization
type: decision
lane: semantic
title: "Pre-1.0 artifacts must meet the current schema"
description: "Before 1.0, Memzoi does not promise backward compatibility for older canonical records, proposals, integrations, or artifacts. A record missing required current schema metadata fails closed and must be manually reviewed then upgraded or removed. Memzoi never silently assigns a safe content class or performs automatic legacy migration."
timestamp: "2026-07-17T23:12:37.026268Z"
updated: "2026-07-17T23:12:37.026268Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: issue
source_ref: "https://github.com/Zokiio/Memzoi/issues/113"
proposal_id: prop_019f711c-8f8a-79b1-b974-da8672c3890d
---

# Pre-1.0 artifacts must meet the current schema

Before 1.0, Memzoi does not promise backward compatibility for older canonical records, proposals, integrations, or artifacts. A record missing required current schema metadata fails closed and must be manually reviewed then upgraded or removed. Memzoi never silently assigns a safe content class or performs automatic legacy migration.
