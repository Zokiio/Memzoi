---
title: Recall and Precheck
---

# Recall and Precheck

Memzoi has three read-side workflows: recall memory for context, build handoff packs for agent switching, and check planned work against risky memories before acting.

## Search active memory

```bash
memzoi search pnpm --json
memzoi search pnpm --type decision --scope-kind repo --limit 5
memzoi search billing --path apps/api/src/billing --json
```

Search is backed by SQLite FTS over active record titles and bodies. It supports optional filters for memory type, scope kind, path prefix, and limit. JSON search results return records with nullable `source_kind`/`source_ref` and citations carrying the four-part provenance metadata described below.

Path filtering matches records bound to the exact path, descendants of the path, or ancestors of the path. That lets a record attached to `apps/web` apply when the user is working in `apps/web/src/App.tsx`.

## Build a context pack

```bash
memzoi context --task "install dependencies" --json
memzoi context --task "edit the frontend" --path apps/web --token-budget 1200
memzoi context --task "resume this task" --include-local --include-session --json
```

Context packs are prompt-ready summaries of task-relevant active memory records. Repo memory (`destination: repo`) is the default and is the only destination queried unless `--include-local` or `--include-session` is supplied. When `--path` is supplied, path-bound records are prioritized. `--token-budget` limits selection before prompt rendering; when omitted, Memzoi uses its default budget.

Local and session memory is not queried, counted, rendered, or exposed unless the caller explicitly opts in. Use `--include-local` and/or `--include-session` only when private runtime continuity should be part of the pack. These flags change the query policy; they do not change the provenance meaning of any returned record.

### Provenance in recall and precheck

Recall and precheck expose four independent pieces of provenance metadata:

- `provenance` is the storage-plane owner, serialized as `git` or `runtime`. `git` means the record is owned by canonical, reviewable `.memzoi/records/*.md` truth; `runtime` means it is runtime-only local or session state. Plane ownership is independent of transport: Git-plane records may be indexed and queried through the derived SQLite database, and SQLite is not canonical memory.
- `destination` is the pre-write routing classification. Recalled records use `repo`, `local`, or `session`: `repo` maps to the Git plane, while `local` and `session` map to the runtime plane. `discard` and `needs_review` are no-write classifications and therefore do not appear as recalled records.
- `source_kind` is optional short source metadata (for example, `human`, `issue`, or `memzoi-local`). It is `null` when the record has no source kind.
- `source_ref` is an optional durable locator for that source (for example, an issue, PR, commit, or URL). It is independent of `source_kind` and is `null` when no reference was recorded.

In JSON, `records[].citations`, top-level `citations`, and `included[].citation` carry this metadata; `included[].provenance` and `included[].destination` repeat the plane and destination for the selected item. `precheck --json` exposes the same citation metadata under each warning. Text prompt lines use the same `provenance=<plane>` and `destination=<destination>` labels, while source metadata remains optional.

The JSON output includes:

- `id`: context pack id
- `task`: requested task
- `prompt`: rendered prompt text
- `records`: selected search results, including context `ranking` metadata
- `citations`: record citations with plane provenance, destination, visibility, and optional `source_kind`/`source_ref`
- `token_budget`: requested token budget, if supplied
- `policy`: requested and included memory destinations
- `budget`: requested budget, effective budget, approximate used budget, and selection/rendering metadata
- `included`: compact metadata for selected records, including provenance and destination
- `omitted`: capped metadata for relevant repo records excluded by budget
- `warnings`: structured notices, currently empty for context ranking
- `next_queries`: targeted follow-up searches, currently empty
- `created_at`: creation timestamp

## Build a handoff pack

```bash
memzoi handoff --task "switch agents during auth work"
memzoi handoff --path crates/memzoi-core --token-budget 800 --json
memzoi handoff --task "resume local task" --include-local --include-session --json
```

Handoff packs are CLI wrappers around context packs for switching agents or harnesses. They reuse the same deterministic context ranking, budget selection, deduplication, provenance, and explicit local/session opt-in policy as `memzoi context`.

`memzoi handoff` requires `--task` or `--path`. When only `--path` is supplied, Memzoi derives the deterministic internal task string `Handoff for path <path>` before building the context pack.

Text output starts with `# Memzoi Handoff`, prints the effective task, optional path, and `Proposal inbox`, then renders the existing context prompt. `Proposal inbox` is backed by the DB-local proposal inbox used by `memzoi proposals` and `memzoi doctor`; it does not scan `.memzoi/proposals/pending`.

JSON output wraps the full context pack under `context`:

- `id`: handoff pack id
- `task`: effective task, including the path-only fallback when used
- `path_prefix`: requested path, if supplied
- `token_budget`, `include_local`, `include_session`: requested handoff options
- `proposal_inbox`: DB-backed open proposal counts with `source: "db"`
- `context`: the full context pack JSON, including `records` with per-record `ranking`, `citations`, `policy`, `budget`, `included`, `omitted`, and `warnings`
- `created_at`: creation timestamp

Local and session memory remains repo-excluded by default. It is not queried, counted, rendered, or exposed in handoff output unless `--include-local` or `--include-session` is supplied.

## Run pre-action checks

Use `precheck` before destructive commands, broad file edits, package-manager changes, migrations, or work that may repeat a known failed attempt.

```bash
memzoi precheck --command "npm install" --json
memzoi precheck --path package.json --action "change package manager"
memzoi precheck --path apps/api/src/billing/invoice.rs --action "change invoice rounding"
```

Precheck searches active memory and returns warnings only for governance memory types:

- `risk`
- `warning`
- `failed_attempt`

Warnings include a severity, cited record id, message, and suggested next step.

## Interpreting warnings

- `risk` produces high-severity warnings and should usually trigger a targeted test or closer review.
- `warning` marks known caveats or repo-specific hazards.
- `failed_attempt` helps agents avoid repeating an approach that already failed.

If there are no matching governance memories, the CLI prints `No memory warnings.` in text mode and returns an empty `warnings` array in JSON mode.
