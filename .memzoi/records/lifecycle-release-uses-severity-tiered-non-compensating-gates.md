---
type: decision
lane: semantic
title: "Lifecycle release uses severity-tiered non-compensating gates"
description: "For #50, lifecycle release decisions are conjunctive and severity-tiered; aggregate scores never compensate for Critical or High failures. Critical covers unauthorized private read/disclosure or Git leakage, stale/altered-plan writes, unauthorized durable/destructive/retention actions, erased-content recovery or resurrection, incomplete live erasure, and automatic repository acceptance. It requires zero observed failures, permits no waiver, and requires a fix or executable proof the affected capability is disabled. High covers cross-route expiry/suppression/retention/session correctness and other behavior that could make an agent use known-invalid memory. Every named invariant must pass; a failing automatic capability is fixed or reduced to a proven safe mode such as report-only. Moderate covers reversible detector quality, false suppression, missed conflicts, review burden, latency, and overhead under predeclared budgets; only exact, time-limited maintainer waivers with mitigation, owner, follow-up, and expiry are allowed. Low findings are reported and trended. Severity, rationale, capabilities, routes, thresholds, sample sizes, fallbacks, detector/policy/schema versions, and corpus cases are frozen before locked evaluation; post-result changes require a new version and release decision. Tests span unit/property, all supported routes, adversarial, fault-injection, migration/recovery, and locked-corpus layers."
timestamp: "2026-07-17T23:14:44.617593Z"
updated: "2026-07-17T23:14:44.617593Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: issue
source_ref: "https://github.com/Zokiio/Memzoi/issues/50"
proposal_id: prop_019f7252-b18a-7410-8ec2-4a4d6822f0d5
---

# Lifecycle release uses severity-tiered non-compensating gates

For #50, lifecycle release decisions are conjunctive and severity-tiered; aggregate scores never compensate for Critical or High failures. Critical covers unauthorized private read/disclosure or Git leakage, stale/altered-plan writes, unauthorized durable/destructive/retention actions, erased-content recovery or resurrection, incomplete live erasure, and automatic repository acceptance. It requires zero observed failures, permits no waiver, and requires a fix or executable proof the affected capability is disabled. High covers cross-route expiry/suppression/retention/session correctness and other behavior that could make an agent use known-invalid memory. Every named invariant must pass; a failing automatic capability is fixed or reduced to a proven safe mode such as report-only. Moderate covers reversible detector quality, false suppression, missed conflicts, review burden, latency, and overhead under predeclared budgets; only exact, time-limited maintainer waivers with mitigation, owner, follow-up, and expiry are allowed. Low findings are reported and trended. Severity, rationale, capabilities, routes, thresholds, sample sizes, fallbacks, detector/policy/schema versions, and corpus cases are frozen before locked evaluation; post-result changes require a new version and release decision. Tests span unit/property, all supported routes, adversarial, fault-injection, migration/recovery, and locked-corpus layers.
