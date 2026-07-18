---
id: canonical-repository-records-require-explicit-content-classification
kind: memory
profile: memzoi
retention: {}
origin:
  origin_key: repository-record:canonical-repository-records-require-explicit-content-classification
  route: repository_materialization
type: warning
lane: semantic
title: "Canonical repository records require explicit content classification"
description: "Checked-in .memzoi/records Markdown must declare content_class: general_repo_knowledge when the content has been reviewed as repository-safe. Missing classifications parse as unknown and fresh derived-index startup correctly refuses admission, which can block commands that open the full service, including memzoi proposals list."
timestamp: "2026-07-17T23:26:01.012257Z"
updated: "2026-07-17T23:26:01.012257Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: test
source_ref: "crates/memzoi-cli/tests/cli_smoke.rs#checked_in_repository_records_allow_fresh_proposals_list_startup"
proposal_id: prop_019f71af-58cf-7921-8e9f-980ad4ac9ddc
---

# Canonical repository records require explicit content classification

Checked-in .memzoi/records Markdown must declare content_class: general_repo_knowledge when the content has been reviewed as repository-safe. Missing classifications parse as unknown and fresh derived-index startup correctly refuses admission, which can block commands that open the full service, including memzoi proposals list.
