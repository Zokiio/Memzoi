# Examples

This directory contains small, copyable examples for Memzoi users and maintainers.

## Files

| File | Purpose |
| --- | --- |
| `compact-canonical-from-proposal.md` | A compact canonical record shape that could result from applying a verbose proposal. |
| `example-memory.md` | A canonical OKF-profile memory record. This file is also used by parser tests, so keep it valid and intentionally small. |
| `memzoi.mcp.json` | A minimal MCP client configuration shape for starting `memzoi-mcp`. |
| `proposals/*.md` | OKF-compatible proposal file examples for create, supersede, and tombstone actions. |

## Memory Record Example

`example-memory.md` shows the authored Markdown/YAML shape Memzoi expects for durable repo memory. Canonical records in a real project should live under:

```text
.memzoi/records/<path-concept-id>.md
```

Use this example to understand the fields, not as a place to store this repository's own memory.

## Proposal Examples

`proposals/*.md` shows the review packet shape for future file-backed proposals under:

```text
.memzoi/proposals/pending/<proposal-id>.md
```

Proposal files may carry review-only context such as reason, confidence, and review notes. Applied canonical records should be compact and should not copy proposal-only metadata unless it remains durable project knowledge.

After review, `memzoi proposal-files apply` moves packets to
`.memzoi/proposals/resolved/applied/`; explicit rejection moves them to
`resolved/rejected/` with a reason. The supersede and tombstone examples each
name one target and require that target to remain active, same-scope, and no
newer than the proposal when applied.

## MCP Config Example

`memzoi.mcp.json` is intentionally generic. Replace `/absolute/path/to/your/repo` with the project root for the repository you want the MCP server to read.

The safer way to generate a real config is:

```bash
memzoi mcp config --project-root .
```

That command resolves the project root to an absolute path so MCP clients can start the server from any working directory.
