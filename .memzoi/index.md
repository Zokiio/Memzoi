# Memory Index

This OKF index file is for human navigation only. It intentionally has no YAML frontmatter and does not define a memory record or proposal.

Canonical Memzoi durable memory files live in:

- `records/` for approved durable records

Current proposed writes are DB-local workflow state under the local runtime directory, usually
`~/.memzoi/projects/<project-key>/memory.db`. File-backed `proposals/` storage is planned for a
later OKF profile slice.

Runtime state is derived:

- `~/.memzoi/projects/<project-key>/memory.db` is a rebuildable SQLite index for approved records
  plus current DB-local proposal workflow state.
- `~/.memzoi/projects/<project-key>/exports/` contains generated projections, not canonical authored
  memory.
