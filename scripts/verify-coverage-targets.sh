#!/bin/sh
set -eu

project_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
listed=$(mktemp)
trap 'rm -f "$listed"' EXIT HUP INT TERM
cargo test --manifest-path "$project_root/Cargo.toml" --all-targets -- --list > "$listed"

tail -n +2 "$project_root/tests/clinic-coverage.tsv" |
while IFS="$(printf '\t')" read -r target tests; do
  old_ifs=$IFS
  IFS=';'
  for test_name in $tests; do
    if ! grep -Fqx "$test_name: test" "$listed"; then
      printf 'Coverage target %s references missing test %s\n' "$target" "$test_name" >&2
      exit 1
    fi
  done
  IFS=$old_ifs
done
printf 'Verified all Clinic coverage targets against executable Rust tests.\n'
