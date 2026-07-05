---
title: Memzoi
sidebar_position: 1
slug: /
---

# Memzoi

Memzoi, pronounced "mem-zoy", is safe project memory for coding agents. It gives agents a repo-local place to recall durable knowledge, propose new memories, check planned work against warnings, export reviewable instruction files, and connect through MCP.

Memzoi v0 is local-first and intended for dogfooding and early experimentation. It is usable from the CLI today; release binaries and package-manager installs are still roadmap items.

## Start here

- [Quickstart](./quickstart.md): run the first workflow in a demo repo.
- [Install](./install.md): install the `memzoi` and `memzoi-mcp` binaries from this repo.
- [Memory lifecycle](./memory-lifecycle.md): propose, review, apply, supersede, or tombstone records.
- [OKF profile](./okf-profile.md): file-native record layout, proposal-state boundary, fields, and apply target flow.
- [Recall and precheck](./recall-and-precheck.md): search memory, build context packs, and check risky work.
- [MCP and agent integration](./mcp-and-agent-integration.md): connect agents and install instruction prompts.
- [Reference](./reference.md): CLI commands, MCP tools, schema values, and limitations.

## What works now

- File-native canonical memory records under `.memzoi/records/`.
- Local runtime state under `~/.memzoi/projects/<project-key>/` for derived SQLite indexes,
  generated exports, and DB-local open proposal state.
- Safe memory lifecycle: propose, approve, reject, apply, supersede, and tombstone.
- Rebuild from canonical records with `memzoi rebuild` when the derived runtime index
  needs to be regenerated.
- Text search and prompt-ready context packs.
- Pre-action governance checks with citations and suggested next steps.
- Deterministic generated exports: OKF Markdown projections, `AGENTS.memory.md`, and `CLAUDE.memory.md`.
- Minimal stdio MCP server with safe tools:
  - `search_memory`
  - `build_context_pack`
  - `propose_memory`
  - `precheck_path`
  - `precheck_action`
  - `precheck_command`

## Design shape

Memzoi separates canonical memory files from derived runtime state.

1. Typed, scoped, versioned durable memory records live under `.memzoi/records/`.
2. A local runtime directory under `~/.memzoi/projects/<project-key>/` holds derived SQLite
   indexes, generated exports, and DB-local pending proposal state.
3. Agent-facing APIs support recall, context building, proposed writes, exports, and pre-action checks.

The project keeps reviewability central: agent writes should be proposed first, human-readable exports should be diffable, and repo-shared memory must not contain secrets or private personal data.
