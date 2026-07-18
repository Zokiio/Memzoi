---
title: Memzoi
sidebar_position: 1
slug: /
---

# Memzoi

Memzoi, pronounced "mem-zoy", is safe project memory for coding agents. It gives agents a repo-local place to recall durable knowledge, propose new memories, check planned work against warnings, export reviewable instruction files, and connect through MCP.

Memzoi v0 is local-first and intended for dogfooding and early experimentation. It is usable from release binaries or from source today; package-manager installs are still roadmap items.

## Start here

- [Quickstart](./quickstart.md): run the first workflow in a demo repo.
- [Install](./install.md): install the `memzoi` and `memzoi-mcp` binaries from this repo.
- [Memory lifecycle](./memory-lifecycle.md): propose, review, apply, supersede, or tombstone records.
- [OKF profile](./okf-profile.md): file-native record and proposal layouts, schema fields, and apply target flow.
- [Recall and precheck](./recall-and-precheck.md): search memory, build context and handoff packs, and check risky work.
- [MCP and agent integration](./mcp-and-agent-integration.md): connect agents and install instruction prompts.
- [Reference](./reference.md): CLI commands, MCP tools, schema values, and limitations.

## What works now

- File-native canonical memory records under `.memzoi/records/`.
- OKF-compatible proposal file schema under `.memzoi/proposals/pending/`.
- Repository runtime state under `~/.memzoi/projects/<repository-key>/`: durable local/session
  memory and proposal state in `shared.db`, plus per-worktree derived indexes and exports.
- Safe memory lifecycle: propose, approve, reject, apply, supersede, and tombstone.
- Rebuild from canonical records with `memzoi rebuild` when the derived runtime index
  needs to be regenerated.
- Text search, prompt-ready context packs, and CLI handoff packs.
- Pre-action governance checks with citations and suggested next steps.
- Deterministic generated exports: OKF Markdown projections, `AGENTS.memory.md`, and `CLAUDE.memory.md`.
- Minimal stdio MCP server with safe tools:
  - `search_memory`
  - `inspect_memory_expiry`
  - `build_context_pack`
  - `plan_capture`
  - `propose_memory`
  - `precheck_path`
  - `precheck_action`
  - `precheck_command`

## Design shape

Memzoi separates canonical memory files from derived runtime state.

1. Typed, scoped, versioned durable memory records live under `.memzoi/records/`.
2. Verbose review packets can live under `.memzoi/proposals/pending/` before becoming compact records.
3. A local runtime directory under `~/.memzoi/projects/<repository-key>/` holds the
   repository-wide `shared.db` authority for local/session memory and proposal state, alongside
   disposable per-worktree indexes and generated exports.
4. Agent-facing APIs support recall, context building, proposed writes, exports, and pre-action checks.

The project keeps reviewability central: agent writes should be proposed first, human-readable exports should be diffable, and repo-shared memory must not contain secrets or private personal data.
