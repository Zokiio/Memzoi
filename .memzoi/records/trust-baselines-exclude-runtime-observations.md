---
id: trust-baselines-exclude-runtime-observations
kind: memory
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
origin:
  version: memzoi/origin-v1
  origin_key: repository-record:trust-baselines-exclude-runtime-observations
  route: repository_materialization
type: decision
lane: semantic
title: "Trust baselines exclude runtime observations"
description: "Keep wall-clock latency and runtime-environment metadata in evaluation reports for diagnosis, but exclude them from exact deterministic baseline comparison. Release gates use documented deterministic quality, safety, integrity, and estimated-usage thresholds; latency gates are opt-in unless a later reviewed evaluation contract explicitly makes them mandatory."
timestamp: "2026-07-17T23:25:52.992237Z"
updated: "2026-07-17T23:25:52.992237Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: issue
source_ref: "https://github.com/Zokiio/Memzoi/issues/47"
proposal_id: prop_019f7264-f453-72b3-8ec9-04b53c1918af
---

# Trust baselines exclude runtime observations

Keep wall-clock latency and runtime-environment metadata in evaluation reports for diagnosis, but exclude them from exact deterministic baseline comparison. Release gates use documented deterministic quality, safety, integrity, and estimated-usage thresholds; latency gates are opt-in unless a later reviewed evaluation contract explicitly makes them mandatory.
