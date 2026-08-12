#!/bin/sh
set -eu

project_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
work_root=$(mktemp -d)
trap 'rm -rf "$work_root"' EXIT HUP INT TERM

actual="$work_root/actual.tsv"
expected="$work_root/expected.tsv"
repositories="$work_root/repositories.tsv"
: > "$actual"

jq -r '.repositories[] | [.name, .url, .commit] | @tsv' \
  "$project_root/tests/clinic-upstream-lock.json" > "$repositories"
while IFS="$(printf '\t')" read -r name url commit; do
  checkout="$work_root/$name"
  git init -q "$checkout"
  git -C "$checkout" remote add origin "$url"
  git -C "$checkout" fetch -q --depth 1 origin "$commit"
  git -C "$checkout" checkout -q --detach FETCH_HEAD
  for test_root in test test-local; do
    if [ -d "$checkout/$test_root" ]; then
      find "$checkout/$test_root" -type f -name '*.test.js' -print |
        sed "s#^$checkout/##" |
        while IFS= read -r path; do printf '%s\t%s\n' "$name" "$path"; done >> "$actual"
    fi
  done
done < "$repositories"
sort -o "$actual" "$actual"

tail -n +2 "$project_root/tests/clinic-baseline.tsv" | cut -f1,2 | sort > "$expected"
diff -u "$expected" "$actual"
printf 'Verified %s pinned Clinic test files.\n' "$(wc -l < "$actual" | tr -d ' ')"
