---
title: Memory Lifecycle
---

# Memory Lifecycle

Memzoi separates agent discovery from durable mutation. New memories are proposed first, then reviewed, approved, and applied.

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

Example JSON shape:

```json
{
  "proposal_id": "prop_...",
  "status": "pending"
}
```

Use short, typed, scoped records. Prefer durable project facts, decisions, procedures, warnings, risks, and failed attempts over raw conversation dumps.

## Approve and apply

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

## Safety policy

Do not store secrets, credentials, raw chat logs, temporary task progress, or private personal data in repo-shared memory. Agent writes should stay proposed and reviewable before they become active records.
