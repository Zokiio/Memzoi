---
title: Recall and Precheck
---

# Recall and Precheck

Memzoi has two read-side workflows: recall memory for context, and check planned work against risky memories before acting.

## Search active memory

```bash
memzoi search pnpm --json
memzoi search pnpm --type decision --scope-kind repo --limit 5
memzoi search billing --path apps/api/src/billing --json
```

Search is backed by SQLite FTS over active record titles and bodies. It supports optional filters for memory type, scope kind, path prefix, and limit.

Path filtering matches records bound to the exact path, descendants of the path, or ancestors of the path. That lets a record attached to `apps/web` apply when the user is working in `apps/web/src/App.tsx`.

## Build a context pack

```bash
memzoi context --task "install dependencies" --json
memzoi context --task "edit the frontend" --path apps/web --token-budget 1200
```

Context packs are prompt-ready summaries of task-relevant active memory records. When `--path` is supplied, path-bound records are prioritized. `--token-budget` limits the rendered prompt text; when omitted, Memzoi uses its default budget.

The JSON output includes:

- `id`: context pack id
- `task`: requested task
- `prompt`: rendered prompt text
- `records`: selected search results
- `citations`: record citations
- `token_budget`: requested token budget, if supplied
- `created_at`: creation timestamp

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
