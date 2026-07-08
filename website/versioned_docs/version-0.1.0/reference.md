---
title: Reference
---

# Reference

This page summarizes Memzoi v0's public CLI, MCP, and model values.

## CLI commands

| Command | Purpose |
| --- | --- |
| `memzoi init` | Initialize repo `.memzoi/` memory and local runtime state. |
| `memzoi propose` | Propose a new memory record. |
| `memzoi approve` | Approve a proposed memory. |
| `memzoi reject` | Reject a proposed memory. |
| `memzoi apply` | Apply an approved memory proposal. |
| `memzoi supersede` | Supersede an active memory record with new content. |
| `memzoi tombstone` | Tombstone an active memory record. |
| `memzoi search` | Search active memory records. |
| `memzoi context` | Build a prompt-ready context pack for a task. |
| `memzoi precheck` | Check planned work against risky memories before acting. |
| `memzoi export` | Export active repo memory into reviewable files. |
| `memzoi rebuild` | Rebuild the derived SQLite database from canonical `.memzoi/records/` files. |
| `memzoi doctor` | Check installation and repo memory readiness. |
| `memzoi quickstart` | Print or run a tiny first-run workflow. |
| `memzoi mcp` | Print MCP integration configuration. |
| `memzoi integrate` | Generate or install agent integration prompts and instructions. |

Run `memzoi <command> --help` for exact options.

## Common command options

| Command | Important options |
| --- | --- |
| `init` | `--force`, `--json` |
| `propose` | `--type`, `--scope-kind`, `--visibility`, `--title`, `--body`, `--actor`, `--json` |
| `approve` | `<proposal-id>`, `--actor`, `--json` |
| `reject` | `<proposal-id>`, `--reason`, `--actor`, `--json` |
| `apply` | `<proposal-id>`, `--actor`, `--json` |
| `supersede` | `<record-id>`, `--type`, `--scope-kind`, `--visibility`, `--title`, `--body`, `--actor`, `--json` |
| `tombstone` | `<record-id>`, `--reason`, `--actor`, `--json` |
| `search` | `<query>`, `--scope-kind`, `--type`, `--path`, `--limit`, `--json` |
| `context` | `--task`, `--path`, `--token-budget`, `--json` |
| `precheck` | `--path`, `--action`, `--command`, `--scope-kind`, `--json` |
| `export` | `<format>`, `--scope-kind`, `--json` |
| `rebuild` | `--json` |
| `doctor` | `--project-root`, `--json` |
| `quickstart` | `--apply-sample`, `--json` |
| `mcp config` | `--project-root` |
| `integrate instructions` | `--file`, `--json` |

## Context JSON

`memzoi context --json` and MCP `build_context_pack` return the prompt-ready pack plus metadata. Existing fields such as `prompt`, `records`, `citations`, and `token_budget` remain available. The additive metadata fields are:

- `budget`: requested budget, effective budget, approximate used budget, and estimate unit.
- `included`: selected records with compact citation, provenance, destination, score, rationale, and estimated size metadata.
- `omitted`: capped repo-record metadata for relevant records excluded by budget.
- `warnings`: structured notices. Local/session runtime matches are reported only as counts because global context content remains repo-only.
- `next_queries`: targeted follow-up queries, currently empty.

## MCP tools

| Tool | Required arguments | Optional arguments |
| --- | --- | --- |
| `search_memory` | `query` | `scope_kind`, `scope`, `type`, `memory_type`, `path`, `path_prefix`, `limit` |
| `build_context_pack` | `task` | `path`, `path_prefix`, `token_budget` |
| `propose_memory` | `title`, `body` | `type`, `memory_type`, `scope_kind`, `scope`, `scope_id`, `visibility`, `tags`, `source_kind`, `source_ref`, `confidence`, `actor` |
| `precheck_path` | `path` | `scope_kind`, `scope` |
| `precheck_action` | `action` | `path`, `scope_kind`, `scope` |
| `precheck_command` | `command` | `path`, `scope_kind`, `scope` |

## Memory types

Valid `--type` and `memory_type` values:

- `fact`
- `preference`
- `decision`
- `procedure`
- `episode`
- `relationship`
- `warning`
- `failed_attempt`
- `risk`
- `instruction_projection`

## Scope kinds

Valid `--scope-kind`, `scope_kind`, and `scope` values:

- `personal`
- `repo`
- `project`
- `team`
- `org`
- `agent`
- `imported_untrusted`

## Visibility values

Valid visibility values:

- `public`
- `private`
- `repo`
- `team`
- `org`

Exports skip `private` records.

## Status values

Record statuses:

- `proposed`
- `active`
- `rejected`
- `superseded`
- `expired`
- `tombstoned`
- `redacted`

Proposal statuses:

- `pending`
- `validated`
- `approved`
- `rejected`
- `applied`

## Export formats

Valid `memzoi export <format>` values:

- `okf`
- `agents-md`
- `claude-md`

## v0 limitations

- Source installs require a Rust/Cargo-capable environment; release binaries do not.
- Search is text/FTS-first, not vector or semantic recall.
- Memory is repo-local; global, personal, team, and org sync are future work.
- `memzoi rebuild` restores approved records from `.memzoi/records/`. Current proposals are DB-local; rebuild refuses to discard readable pending/approved proposals, but a corrupt unreadable DB is treated as derived-cache recovery and may discard DB-local proposal state.
- MCP is intentionally minimal and safe-by-default.
- Homebrew and package-manager installers are not available yet.
