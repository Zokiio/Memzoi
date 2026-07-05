# Examples

This directory contains small, copyable examples for Memzoi users and maintainers.

## Files

| File | Purpose |
| --- | --- |
| `example-memory.md` | A canonical OKF-profile memory record. This file is also used by parser tests, so keep it valid and intentionally small. |
| `memzoi.mcp.json` | A minimal MCP client configuration shape for starting `memzoi-mcp`. |

## Memory Record Example

`example-memory.md` shows the authored Markdown/YAML shape Memzoi expects for durable repo memory. Canonical records in a real project should live under:

```text
.memzoi/records/<path-concept-id>.md
```

Use this example to understand the fields, not as a place to store this repository's own memory.

## MCP Config Example

`memzoi.mcp.json` is intentionally generic. Replace `/absolute/path/to/your/repo` with the project root for the repository you want the MCP server to read.

The safer way to generate a real config is:

```bash
memzoi mcp config --project-root .
```

That command resolves the project root to an absolute path so MCP clients can start the server from any working directory.
