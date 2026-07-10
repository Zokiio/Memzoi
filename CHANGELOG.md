# Changelog

## [0.3.0] - Unreleased

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

[0.3.0]: https://github.com/Zokiio/Memzoi/compare/v0.2.0...v0.3.0
