---
title: Reference
---

# Reference

This page summarizes Memzoi v0's public CLI, MCP, and model values.

## CLI commands

| Command | Purpose |
| --- | --- |
| `memzoi init` | Initialize repo `.memzoi/` memory and local runtime state. |
| `memzoi propose` | Propose a new memory record. Built-in default auto-approves valid proposals but does not apply them. |
| `memzoi proposals` | List, show, and bulk-apply proposal inbox state. |
| `memzoi approve` | Approve a pending or validated memory proposal. |
| `memzoi reject` | Reject a proposed memory. |
| `memzoi apply` | Apply an approved memory proposal into canonical `.memzoi/records/*.md`. |
| `memzoi supersede` | Supersede an active memory record with new content. |
| `memzoi tombstone` | Tombstone an active memory record. |
| `memzoi search` | Search active memory records. |
| `memzoi context` | Build a prompt-ready context pack for a task. |
| `memzoi precheck` | Check planned work against risky memories before acting. |
| `memzoi export` | Export active repo memory into reviewable files. |
| `memzoi rebuild` | Rebuild the derived SQLite database from canonical `.memzoi/records/` files. |
| `memzoi doctor` | Check installation and repo memory readiness. |
| `memzoi quickstart` | Print or run a tiny first-run workflow. |
| `memzoi update` | Check for or apply a Memzoi release update. |
| `memzoi mcp` | Print MCP integration configuration. |
| `memzoi integrate` | Generate or install agent integration prompts and instructions. |

Run `memzoi <command> --help` for exact options.

## Common command options

| Command | Important options |
| --- | --- |
| `init` | `--force`, `--json` |
| `propose` | `--type`, `--scope-kind`, `--visibility`, `--title`, `--body`, `--actor`, `--manual`, `--auto-approve`, `--apply`, `--json` |
| `proposals list` | `--status open\|pending\|validated\|approved\|rejected\|applied\|all`, `--json` |
| `proposals show` | `<proposal-id>`, `--json` |
| `proposals apply` | `--all-approved`, `--actor`, `--json` |
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
| `update` | `--check`, `--ref`, `--json` |
| `mcp config` | `--project-root` |
| `integrate instructions` | `--file`, `--json` |

## Update Command

`memzoi update` checks GitHub Releases and updates supported Mac/Linux release-binary installs. It never installs from branches, SHAs, URLs, or shell scripts. Use `memzoi update --check` to report update state without changing files.

Supported refs:

- `latest`: resolve the latest GitHub release.
- `vX.Y.Z`: install a stable release tag.
- `X.Y.Z`: normalize to `vX.Y.Z`.

JSON status values:

- `up_to_date`
- `update_available`
- `updated`
- `unsupported`
- `invalid_ref`
- `download_failed`
- `checksum_mismatch`
- `rollback_failed`

`--check --json` works from source, Cargo, package-managed, Windows, and CI installs. Apply mode is limited to release-binary installs where `memzoi` and `memzoi-mcp` are sibling binaries in a writable, non-package-managed directory.

## MCP tools

| Tool | Required arguments | Optional arguments |
| --- | --- | --- |
| `search_memory` | `query` | `scope_kind`, `scope`, `type`, `memory_type`, `path`, `path_prefix`, `limit` |
| `build_context_pack` | `task` | `path`, `path_prefix`, `token_budget` |
| `propose_memory` | `title`, `body` | `type`, `memory_type`, `scope_kind`, `scope`, `scope_id`, `visibility`, `tags`, `source_kind`, `source_ref`, `confidence`, `actor`, `approval_mode` |
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

Proposal statuses:

- `pending`: proposal exists but has not been approved.
- `validated`: validation passed, but approval is still required. This state remains supported even when most flows do not create it.
- `approved`: proposal is approved for durable write, but canonical `.memzoi/records/*.md` has not been written.
- `applied`: proposal produced an active canonical record and no longer blocks rebuilds.
- `rejected`: proposal was intentionally closed without applying.
- `open`: synthetic filter meaning `pending`, `validated`, or `approved`.

Record statuses:

- `active`
- `superseded`
- `expired`
- `tombstoned`
- `redacted`

Auto-approval means `approved`, not `applied`.


## Approval policy

Effective proposal approval mode is resolved in this order:

1. Built-in default: `auto`.
2. User-global config: `${MEMZOI_HOME:-~/.memzoi}/config.toml`.
3. Repo config: `.memzoi/config.toml`.
4. CLI or MCP per-call override.

Config shape:

```toml
[workflow]
proposal_approval = "manual" # or "auto"
```

CLI overrides:

- `memzoi propose --manual` creates a `pending` proposal.
- `memzoi propose --auto-approve` forces auto-approval for one proposal.
- `memzoi propose --apply` creates, approves, and applies through the CLI. It is incompatible with `--manual`.

MCP override:

- `propose_memory` accepts `approval_mode: "auto"` or `"manual"`.
- MCP rejects `apply` and `auto_apply`; MCP never writes canonical records.

## Export formats

Valid `memzoi export <format>` values:

- `okf`
- `agents-md`
- `claude-md`

## v0 limitations

- Source installs require a Rust/Cargo-capable environment; release binaries do not.
- Search is text/FTS-first, not vector or semantic recall.
- Memory is repo-local; global, personal, team, and org sync are future work.
- `memzoi rebuild` restores approved records from `.memzoi/records/`. Current proposals are DB-local; rebuild refuses to discard readable open proposals and should be unblocked with `memzoi proposals list --status open`, `memzoi proposals apply --all-approved`, or `memzoi reject <proposal-id> --reason "..."`. A corrupt unreadable DB is treated as derived-cache recovery and may discard DB-local proposal state.
- MCP is intentionally minimal and safe-by-default. It can create proposals under the effective approval policy, but it cannot apply canonical records.
- Homebrew and package-manager installers are not available yet.
