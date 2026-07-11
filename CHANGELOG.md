# Changelog

## [Unreleased]

### Added

- File-native `memzoi eval recall` v2 trust suites with isolated disposable state, search/precheck/context/write-gate cases, lifecycle and scope leakage metrics, citation/provenance checks, token and latency reporting, and an explicit typed baseline for local and CI regression gates.
- Evidence-backed capture with a bounded deterministic one-Markdown profile, redacted secret/PII/transcript blocking, immutable plan and review artifacts, deferred-review lineage, targeted stale-state checks, crash-recoverable proposal/private routing, and provenance that survives canonical apply and rebuild.
- A planning-only MCP 2025-06-18 `plan_capture_v1` tool with bounded stdio, timeout/cancellation, structured output, no memory writes, and private-result denial by default.
- An accepted evidence-backed capture and extractor boundary RFC defining the v0.4 plan/review/apply direction.

## [0.3.1] - 2026-07-10

### Added

- CLI `memzoi expiry` and MCP `inspect_memory_expiry` diagnostics for explaining a record's normal-read eligibility.
- Git-readable resolved proposal packets for reviewed create, reject, supersede, and tombstone actions.
- Separate `proposal_id` lineage alongside nullable evidence `source` and `source_ref` fields.

### Changed

- Record expiry is enforced consistently across normal search, context, handoff, precheck, runtime reads, and generated exports using the inclusive `now >= expires_at` boundary, without rewriting canonical files.
- Canonical apply routes require an explicit `repo-safe` sensitivity; omitted or legacy values resolve to `unknown`, and blocked results are structured and redacted.
- Evidence provenance and proposal lineage are preserved separately through apply, rebuild, recall, audit output, and deterministic exports.

### Fixed

- Path-bound governance records can participate in prechecks without lexical overlap while retaining deterministic ranking, limits, citations, and scope filtering.
- Applying or rejecting file-backed proposals now resolves the packet and synchronizes relational and full-text recall before returning; reported failures roll back affected state, and replay repairs derived drift.
- OKF projection and import now calculate content hashes from the same trimmed body that is persisted.

## [0.3.0] - 2026-07-10

### Added

- Two-plane Git/runtime memory policy with explicit destination, write-route, review, and provenance metadata.
- Local-only runtime records, session checkpoints, session-end promotion, layered context ranking, and handoff packs.
- Review-first classified import planning with guarded repo proposal, local runtime, and session checkpoint writes.
- Deterministic Codex, Claude, and MCP integration profiles generated from the canonical memory policy.
- JSONL event export and expanded proposal-file review/apply workflows.

### Changed

- Recall provenance is now serialized as the storage plane (`git` or `runtime`), while `destination` remains the routing value (`repo`, `local`, or `session`).
- `memzoi proposal-files apply` reports that the runtime index remains stale and directs callers to `memzoi rebuild`; `memzoi doctor` detects this drift.
- Release builds can run as non-uploading cross-platform dry runs and validate version/tag/docs metadata before publishing.

### Fixed

- Downloaded checksum sidecars now use archive basenames and work with standard `shasum -c` verification.
- Pull requests targeting stacked feature branches now run Rust CI.

[0.3.1]: https://github.com/Zokiio/Memzoi/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/Zokiio/Memzoi/compare/v0.2.0...v0.3.0
