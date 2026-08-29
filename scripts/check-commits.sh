#!/bin/sh
set -eu

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <base> <head>" >&2
  exit 2
fi

base=$1
head=$2
pattern='^(feat|fix|doc|docs|test|ci|refactor|perf|chore|revert|style|security)(\([A-Za-z0-9._/-]+\))?(!)?: .*[^[:space:]].*$'
invalid=0

for commit in $(git rev-list --no-merges "$base..$head"); do
  subject=$(git show -s --format=%s "$commit")
  if ! printf '%s\n' "$subject" | grep -Eq "$pattern"; then
    printf 'invalid Conventional Commit: %.12s %s\n' "$commit" "$subject" >&2
    invalid=1
  fi
done

exit "$invalid"
