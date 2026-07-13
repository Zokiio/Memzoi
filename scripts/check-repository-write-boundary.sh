#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"

scan_write_primitives() {
  local scan_root="$1"
  local output="$2"
  : >"$output"
  while IFS= read -r file; do
    case "$file" in
      */repository_io.rs) continue ;;
    esac
    for primitive in \
      'fs::write' \
      'fs::rename' \
      'fs::hard_link' \
      'fs::copy' \
      'OpenOptions::new' \
      'File::create' \
      '.write_all'; do
      count="$(grep -Fo "$primitive" "$file" | wc -l | tr -d ' ' || true)"
      if [ "$count" -gt 0 ]; then
        printf '%s|%s|%s\n' "$file" "$primitive" "$count" >>"$output"
      fi
    done
  done < <(find "$scan_root" -type f -name '*.rs' | sort)
}

actual_writers="$(mktemp)"
fixture_writers="$(mktemp)"
trap 'rm -f "$actual_writers" "$fixture_writers"' EXIT

scan_write_primitives crates/memzoi-core/src "$actual_writers"
if ! diff -u scripts/repository-write-primitives.allow "$actual_writers"; then
  echo "repository mutation primitive inventory changed; centralize the writer or review and update the allowlist" >&2
  exit 1
fi

# This intentionally unsafe fixture must be detected against an empty inventory.
scan_write_primitives scripts/fixtures/repository-write-boundary "$fixture_writers"
if diff -u /dev/null "$fixture_writers" >/dev/null; then
  echo "repository write boundary self-test did not detect the unsafe writer fixture" >&2
  exit 1
fi

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
grep -Fq 'expected_route: RepositoryWriteRoute' crates/memzoi-core/src/repository_io.rs
grep -Fq 'pub const ALL: [Self; 14]' crates/memzoi-core/src/repository_write_safety/policy.rs

echo "repository write boundary structural checks passed"
