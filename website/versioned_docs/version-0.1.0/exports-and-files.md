---
title: Exports and Files
---

# Exports and Files

Memzoi keeps canonical authored memory under repo `.memzoi/records/` and keeps runtime state under
`~/.memzoi/projects/<project-key>/`. Current proposals are DB-local pending state until file-backed
`.memzoi/proposals/` lands in a later slice. Rebuild restores approved records from canonical files
and refuses to discard readable open DB-local proposals. If the runtime database is corrupt or
unreadable, rebuild treats it as a disposable derived cache and may discard DB-local proposal state.
See the [OKF profile](./okf-profile.md) for the file-native source layout.

## Bundle layout

After `memzoi init`, the repo-local memory directory contains:

```text
.memzoi/
  index.md
  records/
```

The local runtime directory contains generated state:

```text
~/.memzoi/projects/<project-key>/
  config.toml
  memory.db
  exports/
```

The default runtime config is:

```toml
version = 1
scope_kind = "repo"

[exports]
okf = "exports/okf"
agents_md = "exports/AGENTS.memory.md"
claude_md = "exports/CLAUDE.memory.md"
```

## Export formats

```bash
memzoi export okf
memzoi export agents-md
memzoi export claude-md
```

`okf` writes a generated projection, not the canonical record source. It emits one Markdown file per
active, non-private memory record for the selected scope. Each export file includes YAML frontmatter
with stable fields such as id, type, scope, visibility, status, confidence, timestamps, source
metadata, content hash, and applicable paths. Canonical authored records under `.memzoi/records/`
use the [OKF profile](./okf-profile.md) fields and are restored by `memzoi rebuild`.

`agents-md` writes an AGENTS-style projection to:

```text
~/.memzoi/projects/<project-key>/exports/AGENTS.memory.md
```

`claude-md` writes a CLAUDE-style projection to:

```text
~/.memzoi/projects/<project-key>/exports/CLAUDE.memory.md
```

Instruction projections include active, non-private records of these types:

- `procedure`
- `decision`
- `warning`
- `risk`

They intentionally skip background fact records that are useful for search but too noisy for always-on agent instructions.

## Generated file policy

Recommended defaults:

- Commit `.memzoi/records/*` when the records are durable repo knowledge.
- Do not commit runtime `memory.db`; it lives under the local Memzoi home directory.
- Keep generated runtime exports out of Git unless explicitly copied into reviewed agent instructions.
- Regenerate exports after memory lifecycle changes with `memzoi export agents-md`, `memzoi export claude-md`, or `memzoi export okf`.

## Scope and privacy

Exports include active records for the selected `--scope-kind`, defaulting to `repo`, and skip records with `private` visibility. Repo-shared memory should not contain secrets or private personal data.
