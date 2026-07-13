# Repository-write safety boundary

Memzoi uses one versioned, fail-closed authorization boundary for candidate-derived repository writes. `repo-safe` is a required classification, not an exemption from inspection.

Before a repository mutation, the route adapter scans typed fields and final serialized projections. The authorization capability is bound with BLAKE3 to the contract and detector versions, route, project identity, explicit authorization proof, freshness inputs, semantic fields, ordered repository-relative paths, target revisions, and exact output bytes. Changing any field, path, filename, revision, or byte invalidates it. The capability has no public constructor.

The deterministic v1 detector registry blocks credential prefixes, private keys, authorization headers, credentialed URLs, service connection strings, cookies and session state, cloud credentials, sensitive environment assignments, JWT-shaped values, bounded high-entropy values, malformed UTF-8, oversized candidates, unsafe paths, and contextual classes that are not general repository knowledge. Typed UUID, digest, and commit fields can opt out of entropy detection; they do not bypass lexical credential detectors.

Blocked diagnostics contain only route and policy versions, logical field locations, stable reason codes, and BLAKE3 fingerprints. They never include the matched text. There is no `--force`, environment variable, or skip-safety bypass.

## Later-edit scanning

The scanner reads complete resulting blobs and never mutates Git:

```sh
memzoi safety scan --file .memzoi/records/example.md
memzoi safety scan --staged --json
memzoi safety scan --range origin/main...HEAD --json
```

Exit codes are `0` for allowed, `2` for blocked, and `1` for operational failure. Staged scans read index blobs; range scans read blobs from the selected head tree. Symlinks, submodules, malformed paths, invalid encoding, and oversized blobs fail closed.

Example pre-commit hook:

```sh
#!/bin/sh
exec memzoi safety scan --staged
```

Example pre-push check:

```sh
#!/bin/sh
exec memzoi safety scan --range origin/main...HEAD
```

This scanner protects later manual edits. It supplements rather than replaces the write capability used by Memzoi routes.
