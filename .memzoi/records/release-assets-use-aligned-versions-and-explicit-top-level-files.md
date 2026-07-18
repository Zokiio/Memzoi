---
id: release-assets-use-aligned-versions-and-explicit-top-level-files
kind: memory
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
origin:
  version: memzoi/origin-v1
  origin_key: repository-record:release-assets-use-aligned-versions-and-explicit-top-level-files
  route: repository_materialization
type: procedure
lane: procedural
title: "Release assets use aligned versions and explicit top-level files"
description: "A Memzoi release tag, workspace package version, and internal memzoi-core dependency versions must agree so installed binaries and the updater report the released version. After artifact download, release upload enumerates top-level regular files explicitly; it must not pass artifact directories or a broad dist glob to GitHub release upload."
timestamp: "2026-07-17T23:24:52.158207Z"
updated: "2026-07-18T00:09:16Z"
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: file
source_ref: "Cargo.toml; .github/workflows/release.yml; crates/memzoi-cli/src/update.rs"
proposal_id: prop_019f7264-9f8c-7321-92e6-82da99a3c766
---

# Release assets use aligned versions and explicit top-level files

A Memzoi release tag, workspace package version, and internal memzoi-core dependency versions must agree so installed binaries and the updater report the released version. After artifact download, release upload enumerates top-level regular files explicitly; it must not pass artifact directories or a broad dist glob to GitHub release upload.
