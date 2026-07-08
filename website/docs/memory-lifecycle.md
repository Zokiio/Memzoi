---
title: Memory Lifecycle
---

# Memory Lifecycle

Memzoi separates agent discovery from durable mutation. New memories are proposed first, then approved, applied, or rejected. In v0.1.1, proposal creation is low-friction: the built-in policy auto-approves valid proposals, but auto-approved still means `approved`, not `applied`. Canonical `.memzoi/records/*.md` files are written only by an explicit apply step.

## Proposal states

Proposal state is separate from canonical record state:

| State | Meaning |
| --- | --- |
| `pending` | Proposal exists but has not been approved. |
| `validated` | Validation passed, but approval is still required. Most flows do not create this state yet, but it remains part of the model. |
| `approved` | Proposal is approved for durable write, but no canonical `.memzoi/records/*.md` file has been written. |
| `applied` | Proposal produced an active canonical record and no longer blocks rebuilds. |
| `rejected` | Proposal was intentionally closed without applying. |
| `open` | Synthetic inbox filter covering `pending`, `validated`, and `approved`. |

Record state is stored on canonical record files. Applied records normally become `active`; later lifecycle commands can mark records `superseded`, `expired`, `tombstoned`, or `redacted`.

## Proposal files

The OKF-compatible proposal file schema is defined for review packets under:

```text
.memzoi/proposals/pending/<proposal-id>.md
```

Proposal files use `status: proposed` and one nested `proposal.action` value: `create`, `supersede`, or `tombstone`. They can include review-only context such as reason, confidence, evidence, and review notes. The applied canonical record under `.memzoi/records/**` should stay compact and does not need to copy proposal-only metadata.

Review file-backed proposals with:

```bash
memzoi proposal-files list
memzoi proposal-files show <proposal-id>
memzoi proposal-files validate
memzoi proposal-files apply <proposal-id>
```

`list`, `show`, and `validate` are read-only. `apply` currently supports only repo-safe `create` proposals with `status: proposed`; it writes a compact canonical record under `.memzoi/records/**`, leaves the pending proposal file in place, and does not update the runtime SQLite index. Run `memzoi rebuild` when runtime search should pick up newly applied Git-plane records. Current CLI/MCP proposal commands still use the runtime proposal inbox states listed above. Accepted/rejected proposal directories, automatic extraction, and local-only runtime memory are separate future lifecycle slices.

Pending proposal files may be committed when they are explicitly intended for PR review and `sensitivity: repo-safe`. Git-plane apply blocks `secret`, `sensitive`, `local-only`, and `unknown` proposals; there is no override flag. Keep blocked sensitivities out of repo-shared commits until a human classifies, sanitizes, or routes them to the future local/runtime plane. Accepted/rejected proposal directories are reserved for a future lifecycle decision; for now, canonical records plus Git history are the durable outcome.

## Destination classification

Destination is a pre-write routing decision for memory candidates. Lane is different: `lane` describes how stored memory is used and retained, while `destination` decides where a candidate is allowed to go before any write happens.

Valid destination values are:

| Destination | Meaning |
| --- | --- |
| `repo` | Durable project knowledge that must become a file-backed proposal before canonical repo memory. |
| `local` | Future private runtime-plane memory that is not committed to the repo. |
| `session` | Future local task-continuity or checkpoint memory. |
| `discard` | Do not write the candidate. |
| `needs_review` | Block automatic writes until a human decides the sharing boundary. |

Examples:

```text
lane: semantic
destination: repo

lane: session
destination: local

lane: procedural
destination: needs_review
```

`team` and `cloud` are reserved for future destination work and are not accepted values yet. Destination classification does not add fields to canonical records or proposal files in this slice.

## Local runtime memory

Local-only records implement the `local` destination in runtime state. They are stored in the project database under `${MEMZOI_HOME:-~/.memzoi}/projects/<project-key>/memory.db`, not in `.memzoi/records/**`.

Use the local namespace for private runtime memory:

```bash
memzoi local add --type preference --title "..." --body "..."
memzoi local list
memzoi local search <query>
```

Local records are marked with `destination: local`, `visibility: private`, and `source_kind: memzoi-local` in JSON output. They are not included in global `memzoi search`, `memzoi context`, or exports yet. Rebuild keeps local runtime rows while restoring canonical repo records from Git.

Promotion from local memory into repo-shared memory must go through later proposal workflows. Local memory should not contain secrets, raw chat logs, or private personal data that should not be retained.

## Approval policy

Effective proposal approval mode is resolved in this order:

1. Built-in default: `auto`.
2. User-global config: `${MEMZOI_HOME:-~/.memzoi}/config.toml`.
3. Repo config: `.memzoi/config.toml`.
4. CLI or MCP per-call override.

Config uses:

```toml
[workflow]
proposal_approval = "manual" # or "auto"
```

`auto` approves valid proposals. It never applies canonical records by itself. Use `manual` when a repo wants every proposal to stay pending until review.

## Create a proposal

```bash
memzoi propose \
  --type decision \
  --scope-kind repo \
  --visibility repo \
  --title "Use pnpm" \
  --body "This repo uses pnpm for package management." \
  --actor "agent:codex" \
  --json
```

Default JSON shape under the built-in `auto` policy:

```json
{
  "proposal_id": "prop_...",
  "status": "approved",
  "validation": {
    "is_valid": true,
    "issues": []
  },
  "applied": false
}
```

Use short, typed, scoped records. Prefer durable project facts, decisions, procedures, warnings, risks, and failed attempts over raw conversation dumps.

## Manual and apply shortcuts

Force a proposal to stay pending:

```bash
memzoi propose --manual \
  --type fact \
  --title "Use pnpm" \
  --body "This repo uses pnpm for package management." \
  --json
```

Create, approve, and immediately apply from the CLI:

```bash
memzoi propose --apply \
  --type decision \
  --title "Use pnpm" \
  --body "This repo uses pnpm for package management." \
  --json
```

`--apply` is a CLI-only shortcut. MCP clients cannot apply canonical records.

## Proposal inbox

Inspect open proposal state before rebuilding or applying:

```bash
memzoi proposals list --status open
memzoi proposals show <proposal-id>
memzoi proposals apply --all-approved
```

The `open` filter includes `pending`, `validated`, and `approved` proposals. `memzoi proposals apply --all-approved` applies only proposals already in `approved`.

## Approve and apply

Manual review can still use explicit lifecycle commands:

```bash
memzoi approve <proposal-id> --actor "reviewer:human" --json
memzoi apply <proposal-id> --actor "agent:applier" --json
```

After `apply`, the proposal becomes an active memory record:

```json
{
  "proposal_id": "prop_...",
  "record_id": "use-pnpm",
  "record_status": "active"
}
```

## Reject

```bash
memzoi reject <proposal-id> \
  --reason "not true for this repo" \
  --actor "reviewer:human" \
  --json
```

Reject proposals that are stale, too broad, duplicated, private, or not actually durable.

## Supersede

```bash
memzoi supersede <record-id> \
  --type decision \
  --scope-kind repo \
  --visibility repo \
  --title "Use pnpm with frozen lockfiles" \
  --body "This repo uses pnpm, and CI should install with the lockfile frozen." \
  --actor "reviewer:human" \
  --json
```

Supersede when the memory is still relevant but needs a replacement record. This preserves lineage instead of silently overwriting the previous record.

## Tombstone

```bash
memzoi tombstone <record-id> \
  --reason "obsolete after package-manager migration" \
  --actor "reviewer:human" \
  --json
```

Tombstone when a record should no longer participate in recall, context packs, prechecks, or exports.

## Rebuild safety

`memzoi rebuild` restores runtime indexes from canonical `.memzoi/records/*.md` files. Because proposal state is DB-local in v0.1.1, rebuild refuses to discard readable open proposals.

Unblock rebuild by closing the proposal inbox:

```bash
memzoi proposals list --status open
memzoi proposals apply --all-approved
memzoi reject <proposal-id> --reason "not durable repo knowledge"
memzoi rebuild
```

If the runtime database is corrupt or unreadable, rebuild treats it as disposable derived state and may discard DB-local proposal state to recover from cache corruption.

## Safety policy

Do not store secrets, credentials, raw chat logs, temporary task progress, or private personal data in repo-shared memory. Agent writes should stay proposed and reviewable before they become active records.
