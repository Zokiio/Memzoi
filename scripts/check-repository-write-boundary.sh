#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"

if rg -n 'okf::create_okf_proposal_file\(' crates/memzoi-core/src --glob '*.rs' --glob '!service.rs' --glob '!okf.rs' >/dev/null; then
  echo "deprecated direct OKF proposal writer remains reachable" >&2
  exit 1
fi
rg -Uq '#\[cfg\(test\)\]\npub\(crate\) fn create_okf_proposal_file' crates/memzoi-core/src/okf.rs

rg -q 'authorization: &AuthorizedRepositoryWriteBatch' crates/memzoi-core/src/repository_io.rs
rg -q 'authorization: &AuthorizedRepositoryWriteBatch' crates/memzoi-core/src/service.rs
rg -q 'repository_io::verify_repository_batch' crates/memzoi-core/src/service.rs
rg -q 'pub const ALL: \[Self; 14\]' crates/memzoi-core/src/repository_write_safety/policy.rs

echo "repository write boundary structural checks passed"
