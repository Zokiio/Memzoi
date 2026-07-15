# Repository-write safety boundary

Memzoi uses one versioned, fail-closed authorization boundary for candidate-derived repository writes. `repo-safe` is a required classification, not an exemption from inspection.

Before a repository mutation, the route adapter scans typed fields and final serialized projections. The authorization capability is bound with BLAKE3 to the contract and detector versions, route, destination, sensitivity, scope, visibility, provenance and content class, project identity, explicit authorization proof, freshness inputs, semantic fields, ordered repository-relative paths, target revisions, and exact output bytes. Changing any policy input, field, path, filename, revision, or byte invalidates it. The capability has no public constructor. Missing contextual classification is `unknown`, never implicitly safe.

The deterministic v1 detector registry blocks credential prefixes, private keys, authorization headers, credentialed URLs, service connection strings, cookies and session state, cloud credentials, sensitive environment assignments, JWT-shaped values, bounded high-entropy values, malformed UTF-8, oversized candidates, unsafe paths, and contextual classes that are not general repository knowledge. Typed UUID, digest, and commit fields can opt out of entropy detection; they do not bypass lexical credential detectors.

Blocked diagnostics contain only route and policy versions, logical field locations, stable reason codes, and BLAKE3 fingerprints. They never include the matched text. There is no `--force`, environment variable, or skip-safety bypass.

At the final mutation seam, repository creation pins the project root and every parent directory by file descriptor, refuses symlink traversal, and creates each destination exclusively without following links. Rollback uses the same pinned parent descriptors. Platforms without equivalent directory-relative no-follow primitives fail closed instead of using a path-based fallback. Candidate-bearing staging and backup files live under the project-scoped local runtime directory, outside the Git worktree; only authorized final projections are installed into the repository.

Capture extractors assign a typed content class from their deterministic evidence policy. Ambiguous candidates remain `unknown`, and a reviewer editing one into the repository route must explicitly classify the reviewed candidate as `general_repo_knowledge`.

`rebuild` validates every canonical record blob with this same managed policy before opening, migrating, deleting, or recreating the runtime index. A prohibited manual edit therefore leaves the existing index untouched.

## Later-edit scanning

The scanner reads complete resulting blobs and never mutates Git:

```sh
memzoi safety scan --file .memzoi/records/example.md
memzoi safety scan --staged --json
memzoi safety scan --range origin/main...HEAD --json
```

Exit codes are `0` for allowed, `2` for blocked, and `1` for operational failure. Staged scans read index blobs; range scans read blobs from the selected head tree. Managed record and proposal metadata is parsed so sensitivity, scope, visibility, and content class all pass through the shared policy; missing, malformed, or prohibited values fail closed. Every blocked report replaces its filename with a fingerprint. Non-UTF-8 paths outside managed memory roots are ignored, while malformed managed paths, symlinks, submodules, invalid encoding, and oversized blobs fail closed.

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
