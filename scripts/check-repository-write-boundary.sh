#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"

if grep -RFn \
  --include='*.rs' \
  --exclude='service.rs' \
  --exclude='okf.rs' \
  'okf::create_okf_proposal_file(' \
  crates/memzoi-core/src >/dev/null; then
  echo "deprecated direct OKF proposal writer remains reachable" >&2
  exit 1
fi
awk '
  previous == "#[cfg(test)]" && index($0, "pub(crate) fn create_okf_proposal_file") == 1 {
    found = 1
  }
  { previous = $0 }
  END { exit found ? 0 : 1 }
' crates/memzoi-core/src/okf.rs

grep -Fq 'authorization: &AuthorizedRepositoryWriteBatch' crates/memzoi-core/src/repository_io.rs
grep -Fq 'authorization: &AuthorizedRepositoryWriteBatch' crates/memzoi-core/src/service.rs
grep -Fq 'repository_io::verify_repository_batch' crates/memzoi-core/src/service.rs
grep -Fq 'pub const ALL: [Self; 14]' crates/memzoi-core/src/repository_write_safety/policy.rs

echo "repository write boundary structural checks passed"
