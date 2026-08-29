#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
rfd_root="${RFD_DIR:-${repo_root}/rfd}"
states='prediscussion|ideation|discussion|published|committed|abandoned'

failures=0

problem() {
  printf '%s\n' "$*" >&2
  failures=$((failures + 1))
}

attribute() {
  local source=$1 name=$2
  sed -n "s/^:${name}:[[:space:]]*//p" "$source"
}

if [[ ! -d $rfd_root ]]; then
  printf 'RFD directory not found: %s\n' "$rfd_root" >&2
  exit 1
fi

printf '%-4s  %-13s  %5s  %-35s  %s\n' RFD State Tasks Title Labels
printf '%-4s  %-13s  %5s  %-35s  %s\n' ---- ------------- ----- ----------------------------------- --------------------

found=0
shopt -s nullglob
entries=("$rfd_root"/*)
shopt -u nullglob

for entry in "${entries[@]}"; do
  entry_name=$(basename "$entry")
  [[ $entry_name == README.md ]] && continue
  if [[ ! -d $entry || ! $entry_name =~ ^[0-9]{4}$ ]]; then
    problem "invalid RFD entry: ${entry_name}"
    continue
  fi

  found=1
  source="$entry/README.adoc"
  if [[ ! -f $source ]]; then
    problem "missing canonical RFD source: ${entry_name}/README.adoc"
    continue
  fi

  number=$(printf '%s\n' "$entry_name" | sed 's/^0*//')
  [[ -n $number ]] || number=0

  authors=$(attribute "$source" authors)
  state=$(attribute "$source" state)
  labels=$(attribute "$source" labels)
  for name in authors state labels; do
    values=$(attribute "$source" "$name")
    count=$(printf '%s\n' "$values" | awk 'NF { count++ } END { print count + 0 }')
    if [[ $count -ne 1 ]]; then
      problem "${entry_name}/README.adoc: document must contain exactly one non-empty ${name} attribute"
    fi
  done
  discussion_lines=$(attribute "$source" discussion)
  discussion_count=$(grep -c '^:discussion:' "$source" || true)
  discussion=${discussion_lines%%$'\n'*}

  if [[ -n $authors && ! $authors =~ \<[^\>]+\> ]]; then
    problem "${entry_name}/README.adoc: authors must include a name and address in angle brackets"
  fi
  if [[ -n $state && ! $state =~ ^($states)$ ]]; then
    problem "${entry_name}/README.adoc: invalid state: ${state}"
  fi
  if [[ $discussion_count -ne 1 ]]; then
    problem "${entry_name}/README.adoc: document must contain exactly one discussion attribute"
  elif [[ -n $discussion && ! $discussion =~ ^https?:// ]]; then
    problem "${entry_name}/README.adoc: discussion must be empty or an HTTP(S) URL"
  elif [[ $state =~ ^(discussion|published|committed)$ && -z $discussion ]]; then
    problem "${entry_name}/README.adoc: state ${state} requires a discussion URL"
  fi

  title_lines=$(grep -E '^= RFD [0-9]+ .+' "$source" || true)
  title_count=$(printf '%s\n' "$title_lines" | awk 'NF { count++ } END { print count + 0 }')
  if [[ $title_count -ne 1 ]]; then
    problem "${entry_name}/README.adoc: document must contain exactly one RFD title"
    title='(missing title)'
  else
    title_number=${title_lines#= RFD }
    title_number=${title_number%% *}
    title=${title_lines#= RFD "$title_number" }
    if [[ $title_number != "$number" ]]; then
      problem "${entry_name}/README.adoc: title number ${title_number} does not match directory number ${number}"
    fi
  fi

  implementations=()
  [[ -f $entry/IMPLEMENTATION.org ]] && implementations+=("$entry/IMPLEMENTATION.org")
  [[ -f $entry/IMPLEMENTATION.md ]] && implementations+=("$entry/IMPLEMENTATION.md")
  task_summary=-
  if [[ ${#implementations[@]} -eq 0 ]]; then
    problem "missing implementation checklist: ${entry_name}/IMPLEMENTATION.org or IMPLEMENTATION.md"
  elif [[ ${#implementations[@]} -gt 1 ]]; then
    problem "multiple implementation checklist formats: ${entry_name}"
  else
    implementation=${implementations[0]}
    implementation_name=$(basename "$implementation")
    read -r total completed < <(
      awk '/^[[:space:]]*[-+*][[:space:]]+\[[ xX]\][[:space:]]/ {
             total++; if ($0 ~ /^[[:space:]]*[-+*][[:space:]]+\[[xX]\][[:space:]]/) completed++
           } END { printf "%d %d\n", total, completed }' "$implementation"
    )
    task_summary="${completed}/${total}"
    if [[ $implementation_name == IMPLEMENTATION.org ]]; then
      expected_heading="#+TITLE: RFD ${entry_name} implementation checklist"
      backlink='[[file:README.adoc]['
    else
      expected_heading="# RFD ${entry_name} implementation checklist"
      backlink='](README.adoc)'
    fi
    if [[ $(head -n 1 "$implementation") != "$expected_heading" ]]; then
      problem "invalid implementation checklist heading: ${entry_name}/${implementation_name}"
    fi
    if ! grep -Fq "$backlink" "$implementation"; then
      problem "implementation checklist must link to its RFD: ${entry_name}/${implementation_name}"
    fi
    if ! grep -Fq "link:${implementation_name}[" "$source"; then
      problem "RFD must link to its implementation checklist: ${entry_name}/README.adoc"
    fi
  fi

  if grep -Eq '^\* \[[ xX]\]' "$source"; then
    problem "implementation checkboxes belong in a separate implementation document: ${entry_name}/README.adoc"
  fi

  printf '%-4s  %-13s  %5s  %-35s  %s\n' "$entry_name" "${state:-\(missing\)}" "$task_summary" "$title" "${labels:-\(missing labels\)}"
done

if [[ $found -eq 0 ]]; then
  printf 'no RFDs found in %s\n' "$rfd_root" >&2
  exit 1
fi
if [[ $failures -gt 0 ]]; then
  printf '\nRFD status check failed with %s issue(s).\n' "$failures" >&2
  exit 1
fi

printf '\nRFD status check passed.\n'
