---
id: source-installer-can-verify-a-shadowed-binary-directory
kind: memory
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
origin:
  version: memzoi/origin-v1
  origin_key: repository-record:source-installer-can-verify-a-shadowed-binary-directory
  route: repository_materialization
type: warning
lane: semantic
title: "Source installer can verify a shadowed binary directory"
description: "For a source install with neither MEMZOI_INSTALL_DIR nor CARGO_INSTALL_ROOT, cargo install writes to Cargo's default bin directory while scripts/install.sh derives its verification path as ~/.local/bin. A stale ~/.local/bin copy can therefore make a successful source upgrade appear to report an old version. Align the source-install root and verification path."
timestamp: "2026-07-17T23:25:45.763425Z"
updated: "2026-07-17T23:25:45.763425Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: file
source_ref: scripts/install.sh
proposal_id: prop_019f7264-e70e-72a1-8ddf-4c34add3ddd3
---

# Source installer can verify a shadowed binary directory

For a source install with neither MEMZOI_INSTALL_DIR nor CARGO_INSTALL_ROOT, cargo install writes to Cargo's default bin directory while scripts/install.sh derives its verification path as ~/.local/bin. A stale ~/.local/bin copy can therefore make a successful source upgrade appear to report an old version. Align the source-install root and verification path.
