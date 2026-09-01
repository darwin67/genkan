#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
checker="$repo_root/scripts/check-rfd-status.sh"
test_root=$(mktemp -d "${TMPDIR:-/tmp}/genkan-rfd-check.XXXXXX")
rfd_root="$test_root/rfd"
output="$test_root/output"
trap 'rm -rf "$test_root"' EXIT

reset_fixtures() {
  rm -rf "$rfd_root"
  mkdir -p "$rfd_root"
  printf '= Test RFDs\n' > "$rfd_root/README.adoc"
}

run_success() {
  local expected=${1:-}
  if ! RFD_DIR="$rfd_root" bash "$checker" > "$output" 2>&1; then
    cat "$output" >&2
    printf 'expected RFD checker to pass\n' >&2
    exit 1
  fi
  if [[ -n $expected ]] && ! grep -Fq "$expected" "$output"; then
    cat "$output" >&2
    printf 'RFD checker output did not include: %s\n' "$expected" >&2
    exit 1
  fi
}

run_failure() {
  local expected=$1
  if RFD_DIR="$rfd_root" bash "$checker" > "$output" 2>&1; then
    cat "$output" >&2
    printf 'expected RFD checker to fail with: %s\n' "$expected" >&2
    exit 1
  fi
  if ! grep -Fq "$expected" "$output"; then
    cat "$output" >&2
    printf 'RFD checker failure did not include: %s\n' "$expected" >&2
    exit 1
  fi
}

write_valid_rfd() {
  local state=$1 discussion=$2 implementation_format=${3:-org} implementation_name
  case "$implementation_format" in
    org) implementation_name=IMPLEMENTATION.org ;;
    md) implementation_name=IMPLEMENTATION.md ;;
    *) printf 'unsupported test implementation format: %s\n' "$implementation_format" >&2; exit 1 ;;
  esac
  mkdir -p "$rfd_root/0001"
  cat > "$rfd_root/0001/README.adoc" <<EOF
:authors: Example Author <author@example.com>
:state: ${state}
:discussion: ${discussion}
:labels: software, process

= RFD 1 Valid RFD

== Implementation

See link:${implementation_name}[implementation checklist].
EOF
  if [[ $implementation_format == org ]]; then
    cat > "$rfd_root/0001/$implementation_name" <<'EOF'
#+TITLE: RFD 0001 implementation checklist

Implements [[file:README.adoc][RFD 1: Valid RFD]].

- [ ] Complete the work.
EOF
  else
    cat > "$rfd_root/0001/$implementation_name" <<'EOF'
# RFD 0001 implementation checklist

Implements [RFD 1: Valid RFD](README.adoc).

- [ ] Complete the work.
EOF
  fi
}

reset_fixtures; write_valid_rfd discussion https://example.com/pull/1
cat >> "$rfd_root/0001/IMPLEMENTATION.org" <<'EOF'
- [X] Finished task.
  - [x] Finished nested task.
EOF
run_success "0001  discussion       2/3"

sed -i.bak 's/= RFD 1 Valid RFD/= RFD 1 A deliberately long title that exceeds the minimum table width/' \
  "$rfd_root/0001/README.adoc"
rm "$rfd_root/0001/README.adoc.bak"
run_success
header=$(grep '^RFD ' "$output")
row=$(grep '^0001 ' "$output")
header_before_labels=${header%%Labels*}
row_before_labels=${row%%software, process*}
if [[ ${#header_before_labels} -ne ${#row_before_labels} ]]; then
  cat "$output" >&2
  printf 'RFD checker Labels column is not aligned\n' >&2
  exit 1
fi

reset_fixtures; write_valid_rfd prediscussion "" md
run_success "0001  prediscussion    0/1"

reset_fixtures; write_valid_rfd prediscussion ""
rm "$rfd_root/0001/IMPLEMENTATION.org"
run_failure "missing implementation checklist"

reset_fixtures; write_valid_rfd prediscussion ""
for marker in '*' '-' '+'; do
  printf '\n%s [ ] This belongs in the implementation document.\n' "$marker" >> "$rfd_root/0001/README.adoc"
  run_failure "implementation checkboxes belong in a separate implementation document"
  sed -i.bak '$d' "$rfd_root/0001/README.adoc"
  rm "$rfd_root/0001/README.adoc.bak"
done

reset_fixtures; write_valid_rfd prediscussion ""
printf '# RFD 0001 implementation checklist\n\nImplements [RFD 1](README.adoc).\n' > "$rfd_root/0001/IMPLEMENTATION.md"
run_failure "multiple implementation checklist formats"

reset_fixtures; write_valid_rfd draft ""
run_failure "invalid state: draft"

reset_fixtures; write_valid_rfd discussion ""
run_failure "state discussion requires a discussion URL"

reset_fixtures; write_valid_rfd prediscussion ""
printf '\n:authors:\n' >> "$rfd_root/0001/README.adoc"
run_failure "exactly one authors attribute"

reset_fixtures; write_valid_rfd prediscussion ""
printf '\n:discussion:\n' >> "$rfd_root/0001/README.adoc"
run_failure "exactly one discussion attribute"

reset_fixtures; write_valid_rfd prediscussion ""
sed -i.bak '1c\:authors: Example Author <author@example.com>; Missing Address' "$rfd_root/0001/README.adoc"
rm "$rfd_root/0001/README.adoc.bak"
run_failure "every author must include a name and address in angle brackets"

reset_fixtures; write_valid_rfd prediscussion ""
sed -i.bak '/^:labels:/d' "$rfd_root/0001/README.adoc"
rm "$rfd_root/0001/README.adoc.bak"
run_failure "exactly one labels attribute"

reset_fixtures; write_valid_rfd prediscussion ""
sed -i.bak $'s/:labels: /:labels:\t/' "$rfd_root/0001/README.adoc"
rm "$rfd_root/0001/README.adoc.bak"
run_failure "attribute values must not contain tabs"

reset_fixtures; write_valid_rfd prediscussion ""
{
  sed -n '2p' "$rfd_root/0001/README.adoc"
  sed -n '1p' "$rfd_root/0001/README.adoc"
  sed -n '3,$p' "$rfd_root/0001/README.adoc"
} > "$rfd_root/0001/README.adoc.new"
mv "$rfd_root/0001/README.adoc.new" "$rfd_root/0001/README.adoc"
run_failure "must start with the canonical RFD header"

reset_fixtures; write_valid_rfd prediscussion ftp://example.com/discussion
run_failure "discussion must be empty or an HTTP(S) URL"

reset_fixtures; write_valid_rfd prediscussion http://
run_failure "discussion must be empty or an HTTP(S) URL"

reset_fixtures; write_valid_rfd prediscussion ""
sed -i.bak '1s/checklist/list/' "$rfd_root/0001/IMPLEMENTATION.org"
rm "$rfd_root/0001/IMPLEMENTATION.org.bak"
run_failure "invalid implementation checklist heading"

reset_fixtures; write_valid_rfd prediscussion ""
sed -i.bak '/link:IMPLEMENTATION.org/d' "$rfd_root/0001/README.adoc"
rm "$rfd_root/0001/README.adoc.bak"
run_failure "RFD must link to its implementation checklist"

reset_fixtures; write_valid_rfd prediscussion ""
sed -i.bak 's/\[\[file:README.adoc\]/[[file:OTHER.adoc]/' "$rfd_root/0001/IMPLEMENTATION.org"
rm "$rfd_root/0001/IMPLEMENTATION.org.bak"
run_failure "implementation checklist must link to its RFD"

reset_fixtures; write_valid_rfd prediscussion ""
sed -i.bak 's/= RFD 1 /= RFD 2 /' "$rfd_root/0001/README.adoc"
rm "$rfd_root/0001/README.adoc.bak"
run_failure "does not match directory number 1"

reset_fixtures
mkdir -p "$rfd_root/1"
printf '= RFD 1 Invalid directory\n' > "$rfd_root/1/README.adoc"
run_failure "invalid RFD entry"

reset_fixtures
run_failure "no RFDs found"

printf 'RFD checker tests passed.\n'
