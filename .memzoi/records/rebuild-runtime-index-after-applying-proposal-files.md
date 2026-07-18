---
type: procedure
lane: procedural
title: "Rebuild runtime index after applying proposal files"
description: "memzoi proposal-files apply writes canonical .memzoi/records but does not update derived SQLite state. Run memzoi rebuild before using search or context. memzoi doctor reports this drift and recommends rebuild."
timestamp: "2026-07-10T05:36:16.14076Z"
updated: "2026-07-18T00:34:07Z"
status: superseded
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: cli
source_ref: prop_019f4a86-cd01-7993-8ff1-57a513dba32f
---

# Rebuild runtime index after applying proposal files

memzoi proposal-files apply writes canonical .memzoi/records but does not update derived SQLite state. Run memzoi rebuild before using search or context. memzoi doctor reports this drift and recommends rebuild.
