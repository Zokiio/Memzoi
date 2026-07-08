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
| `memzoi proposal-files` | List, show, validate, and apply OKF proposal files under `.memzoi/proposals/pending/`. |
| `memzoi local` | Add, list, and search local-only runtime memory records. |
| `memzoi checkpoint` | Add and list runtime session checkpoints. |
| `memzoi session-end` | Promote explicit structured session-end candidates into proposal files or runtime memory. |
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
| `proposal-files list` | `--json` |
| `proposal-files show` | `<proposal-id>`, `--json` |
| `proposal-files validate` | `--json` |
| `proposal-files apply` | `<proposal-id>`, `--json` |
| `local add` | `--type`, `--title`, `--body`, `--actor`, `--json` |
| `local list` | `--json` |
| `local search` | `<query>`, `--limit`, `--json` |
| `checkpoint add` | `--task`, `--note` or `--from-file`, `--actor`, `--json` |
| `checkpoint list` | `--json` |
| `session-end` | `--from-file <path>` or `--from-checkpoint <checkpoint-id>`, `--actor`, `--json` |
| `approve` | `<proposal-id>`, `--actor`, `--json` |
| `reject` | `<proposal-id>`, `--reason`, `--actor`, `--json` |
| `apply` | `<proposal-id>`, `--actor`, `--json` |
| `supersede` | `<record-id>`, `--type`, `--scope-kind`, `--visibility`, `--title`, `--body`, `--actor`, `--json` |
| `tombstone` | `<record-id>`, `--reason`, `--actor`, `--json` |
| `search` | `<query>`, `--scope-kind`, `--type`, `--path`, `--limit`, `--json` |
| `context` | `--task`, `--path`, `--token-budget`, `--include-local`, `--include-session`, `--json` |
| `precheck` | `--path`, `--action`, `--command`, `--scope-kind`, `--json` |
| `export` | `<format>`, `--scope-kind`, `--json` |
| `rebuild` | `--json` |
| `doctor` | `--project-root`, `--json` |
| `quickstart` | `--apply-sample`, `--json` |
| `update` | `--check`, `--ref`, `--json` |
| `mcp config` | `--project-root` |
| `integrate instructions` | `--file`, `--json` |

## Context JSON

`memzoi context --json` and MCP `build_context_pack` return the prompt-ready pack plus metadata. Existing fields such as `prompt`, `records`, `citations`, and `token_budget` remain available. The additive metadata fields are:

- `budget`: requested budget, effective budget, approximate used budget, and estimate unit.
- `included`: selected records with compact citation, provenance, destination, score, rationale, and estimated size metadata.
- `omitted`: capped repo-record metadata for relevant records excluded by budget.
- `warnings`: structured notices. Local/session runtime matches are reported only as counts because global context content remains repo-only.
- `next_queries`: targeted follow-up queries, currently empty.

## Update Command

`memzoi update` checks GitHub Releases and updates supported Mac/Linux release-binary installs. Automatic apply mode never installs from branches, SHAs, URLs, or shell scripts; unsupported installs may print manual commands that use the official install scripts. Use `memzoi update --check` to report update state without changing files.

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
| `build_context_pack` | `task` | `path`, `path_prefix`, `token_budget`, `include_local`, `include_session` |
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

## Memory lanes

Valid record `lane` values:

- `session`
- `semantic`
- `episodic`
- `procedural`

Records without `lane` remain valid and are treated as `semantic`. `lane` is separate from `type`: lane describes memory usage and retention, while type describes the record content.

## Proposal file schema values

OKF-compatible proposal files live under `.memzoi/proposals/pending/*.md`. They are review packets and use:

```yaml
status: proposed
proposal:
  action: create
```

Valid proposal file actions:

- `create`
- `supersede`
- `tombstone`

`supersede` proposals require at least one `supersedes` target. `tombstone` proposals require `proposal.target`. `update` is intentionally unsupported in the first file profile.

Valid proposal sensitivity values:

- `repo-safe`
- `local-only`
- `sensitive`
- `secret`
- `unknown`

The current CLI/MCP proposal inbox remains DB-local workflow state and uses the operational proposal statuses below.

Proposal file review commands:

```bash
memzoi proposal-files list
memzoi proposal-files show <proposal-id>
memzoi proposal-files validate
memzoi proposal-files apply <proposal-id>
```

`list`, `show`, and `validate` are read-only. `apply` currently supports only `proposal.action: create` with `status: proposed` and `sensitivity: repo-safe`; it writes a compact canonical record under `.memzoi/records/`, leaves the pending proposal file in place, and does not update runtime SQLite state. Run `memzoi rebuild` when the runtime search/index needs to reflect newly applied Git-plane records.

Git-plane apply blocks `secret`, `sensitive`, `local-only`, and `unknown` proposals, and there is no override flag. Classify or sanitize blocked proposals before repo apply, or route `local-only` memory to the local/runtime plane.

## Memory destinations

Destination is a pre-write classification for memory candidates. It is separate from `lane`: `destination` decides where a candidate may go, while `lane` describes how stored memory is used and retained.

Valid destination values:

- `repo`
- `local`
- `session`
- `discard`
- `needs_review`

`repo` candidates must become file-backed proposals before canonical repo memory. `local` and `session` are runtime-plane destinations by default. `discard` means no write. `needs_review` blocks automatic writes until a human decides. `team` and `cloud` are future destinations and are not accepted values yet.

## Local runtime memory

Local memory commands:

```bash
memzoi local add --type preference --title "..." --body "..."
memzoi local list
memzoi local search <query>
```

Local records are stored in the runtime project database under `${MEMZOI_HOME:-~/.memzoi}/projects/<project-key>/memory.db`. They are marked as `destination: local`, `visibility: private`, and `source_kind: memzoi-local` in JSON output.

Local records are not written to `.memzoi/records/**`, are not returned by global `memzoi search`, and are not exported into repo-shared agent files. `memzoi context` is repo-only by default and includes local records only with `--include-local`. Use later proposal workflows to promote local memory into repo-shared memory.

## Session checkpoints

Checkpoint commands:

```bash
memzoi checkpoint add --task "..." --note "..."
memzoi checkpoint add --task "..." --from-file notes.md
memzoi checkpoint list
```

Checkpoints are stored in the runtime project database under `${MEMZOI_HOME:-~/.memzoi}/projects/<project-key>/memory.db`. They are marked as `destination: session`, `lane: session`, `type: episode`, `visibility: private`, and `source_kind: memzoi-checkpoint` in JSON output.

Checkpoints store only explicit `--note` or `--from-file` content. They are not written to `.memzoi/records/**`, are not returned by global `memzoi search`, and are not exported into repo-shared agent files. `memzoi context` is repo-only by default and includes checkpoints only with `--include-session`. Use later session-end proposal workflows to promote durable findings into repo memory.

## Session-end promotion

Session-end promotion reads only explicit structured YAML, either from a file or from an existing checkpoint body:

```bash
memzoi session-end --from-file notes.yml
memzoi session-end --from-checkpoint <checkpoint-id>
```

The input must include a `task` and a `candidates` list:

```yaml
task: "Implement auth middleware"
candidates:
  - destination: repo
    type: decision
    lane: semantic
    title: Protected routes validate sessions server-side
    body: Protected routes must validate sessions server-side.
    sensitivity: repo-safe
    reason: Learned while implementing middleware.
    scope:
      kind: repo
      paths:
        - src/auth/**
    tags:
      - auth
      - security
```

Memzoi validates the whole batch and prepares repo proposal files before writing. `repo` candidates must be `repo-safe` and become pending `.memzoi/proposals/pending/*.md` proposal files only; they are not applied and do not write canonical `.memzoi/records/*.md` files. `local` candidates create private runtime records. `session` candidates create runtime checkpoint records. Runtime row writes are transactional, and created proposal files are cleaned up if a later promotion step fails. `discard` and `needs_review` candidates create no writes.

`session-end` does not inspect transcripts, chat logs, shell history, hidden agent state, or context packs. Free-text notes and checkpoints are rejected until a future extraction workflow exists.

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

Operational proposal inbox statuses:

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
- `memzoi rebuild` restores approved records from `.memzoi/records/`. Current proposals are DB-local; rebuild refuses to discard readable open proposals and should be unblocked with `memzoi proposals list --status open`, `memzoi proposals apply --all-approved`, or `memzoi reject <proposal-id> --reason "..."`. A corrupt unreadable DB causes rebuild to fail before deleting local or session runtime rows.
- MCP is intentionally minimal and safe-by-default. It can create proposals under the effective approval policy, but it cannot apply canonical records.
- Homebrew and package-manager installers are not available yet.
