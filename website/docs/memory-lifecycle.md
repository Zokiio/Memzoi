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
