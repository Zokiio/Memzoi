# Agent instructions

This project explores file-native AI memory systems.

Guidelines:

- Treat Markdown/YAML files as the source of truth.
- Runtime indexes should be generated and disposable.
- Prefer small, typed, scoped memory records over large unstructured dumps.
- Preserve human readability and Git diffability.
- Do not store secrets or private personal data in repo-shared memory.
- Agent writes should be proposed and reviewable before being applied.

<!-- memzoi:start -->
## Memzoi

You are working in a repo that uses Memzoi.

Before non-trivial work:
- Run `memzoi context --task "<task>"`.
- If editing specific files, include `--path <relative/path>`.

Before risky actions:
- Run `memzoi precheck --path <relative/path>`.
- Run `memzoi precheck --command "<command>"` before destructive or broad commands.

When durable repo knowledge is discovered:
- Propose it with `memzoi propose --type <type> --title "<title>" --body "<body>"`.
- Use types like fact, decision, procedure, preference, warning, risk, or failed_attempt.
- Prefer proposals over silent durable mutation.

Do not store secrets, raw chat logs, temporary task progress, or private user facts in repo memory.
<!-- memzoi:end -->
