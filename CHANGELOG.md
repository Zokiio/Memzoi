# Changelog

## [0.5.0] - 2026-07-12

### Added

- Candidate-neutral recall-v3 evaluation with strict public/locked corpus
  contracts, digest commitments, isolated production lexical-baseline runs, and
  stable machine-readable reports.
- Offline manifest-driven exact-vector candidate validation for semantic-only,
  lexical-reranking, and lexical/semantic-union experiments, with shared
  eligibility, citation, context-budget, and lexical-fallback boundaries.
- Candidate-bound operational, task-utility, privacy-safe trace, deterministic
  workload, and two-track competitor-evidence validators.

### Changed

- The v0.5 roadmap now treats semantic recall as an eval-gated decision. Any
  accepted profile remains repository-only, offline, single-profile, and opt-in;
  default promotion is not authorized by this release.

### Known limitations

- The checked-in recall-v3 candidate, operational, and competitor inputs are
  synthetic contract fixtures. They validate the evaluation harness and are not
  evidence that semantic or hybrid recall should ship.
- Semantic retrieval remains undecided. Memzoi continues to use lexical recall
  in normal product operation without model installation, network access, or an
  embedding index.

## [0.4.0] - 2026-07-11

### Added

- File-native `memzoi eval recall` v2 trust suites with isolated disposable state, search/precheck/context/write-gate cases, lifecycle and scope leakage metrics, citation/provenance checks, token and latency reporting, and an explicit typed baseline for local and CI regression gates.
- Evidence-backed capture with a bounded deterministic one-Markdown profile, redacted secret/PII/transcript blocking, immutable plan and review artifacts, deferred-review lineage, targeted stale-state checks, crash-recoverable proposal/private routing, and provenance that survives canonical apply and rebuild.
- Deterministic instruction-file, ADR, and Git-change capture profiles with exact semantic evidence, generated-content exclusion, status-aware routing, bounded directory discovery, explicit supplied-byte replay, and immutable Git-range sourcing.
- File-native `memzoi eval capture` v1 suites with isolated fixtures, per-profile quality metrics, prohibited-output and mutation hard gates, human-review burden limits, and exact deterministic baseline enforcement in CI.
- A planning-only MCP 2025-06-18 `plan_capture_v1` tool with bounded stdio, timeout/cancellation, structured output, no memory writes, and private-result denial by default.
- An accepted evidence-backed capture and extractor boundary RFC defining the v0.4 plan/review/apply direction.

### Known limitations

- CLI and MCP capture file operations fail closed on Windows because v0.4.0 requires Unix handle-relative, no-symlink file access. Windows release binaries remain available for the rest of the CLI and MCP surface.

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

[0.5.0]: https://github.com/Zokiio/Memzoi/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/Zokiio/Memzoi/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/Zokiio/Memzoi/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/Zokiio/Memzoi/compare/v0.2.0...v0.3.0
