#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
rfd_root="${RFD_DIR:-${repo_root}/rfd}"
states='prediscussion|ideation|discussion|published|committed|abandoned'
discussion_pattern='^https?://[^/?#[:space:]]+([/?#][^[:space:]]*)?$'

failures=0
row_rfds=()
row_states=()
row_tasks=()
row_titles=()
row_labels=()
title_width=35

problem() {
  printf '%s\n' "$*" >&2
  failures=$((failures + 1))
}

attribute() {
  local source=$1 name=$2
  awk -v prefix=":${name}:" '
    index($0, prefix) == 1 {
      sub("^" prefix "[[:space:]]*", "")
      print
      exit
    }
  ' "$source"
}

valid_authors() {
  awk -v authors="$1" 'BEGIN {
    count = split(authors, owners, ";")
    if (count == 0) exit 1
    for (i = 1; i <= count; i++) {
      owner = owners[i]
      sub(/^[[:space:]]*/, "", owner)
      sub(/[[:space:]]*$/, "", owner)
      if (owner !~ /^[^<>]+[[:space:]]<[^<>]+>$/) exit 1
    }
  }'
}

if [[ ! -d $rfd_root ]]; then
  printf 'RFD directory not found: %s\n' "$rfd_root" >&2
  exit 1
fi

found=0
shopt -s nullglob
entries=("$rfd_root"/*)
shopt -u nullglob

for entry in "${entries[@]}"; do
  entry_name=$(basename "$entry")
  [[ $entry_name == README.adoc ]] && continue
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

  mapfile -t header < <(head -n 6 "$source")
  if [[ ! ${header[0]-} =~ ^:authors:[[:space:]]+.+$ ]] ||
     [[ ! ${header[1]-} =~ ^:state:[[:space:]]+.+$ ]] ||
     [[ ! ${header[2]-} =~ ^:discussion:[[:space:]]*.*$ ]] ||
     [[ ! ${header[3]-} =~ ^:labels:[[:space:]]+.+$ ]] ||
     [[ -n ${header[4]-} ]] ||
     [[ ! ${header[5]-} =~ ^\=\ RFD\ [0-9]+\ .+$ ]]; then
    problem "${entry_name}/README.adoc: document must start with the canonical RFD header"
  fi
  if head -n 4 "$source" | grep -q $'\t'; then
    problem "${entry_name}/README.adoc: attribute values must not contain tabs"
  fi

  for name in authors state discussion labels; do
    count=$(grep -c "^:${name}:" "$source" || true)
    if [[ $count -ne 1 ]]; then
      problem "${entry_name}/README.adoc: document must contain exactly one ${name} attribute"
    fi
  done

  authors=$(attribute "$source" authors)
  state=$(attribute "$source" state)
  discussion=$(attribute "$source" discussion)
  labels=$(attribute "$source" labels)

  if ! valid_authors "$authors"; then
    problem "${entry_name}/README.adoc: every author must include a name and address in angle brackets"
  fi
  if [[ -n $state && ! $state =~ ^($states)$ ]]; then
    problem "${entry_name}/README.adoc: invalid state: ${state}"
  fi
  if [[ -n $discussion && ! $discussion =~ $discussion_pattern ]]; then
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

  if grep -Eq '^[[:space:]]*[-+*][[:space:]]+\[[ xX]\][[:space:]]' "$source"; then
    problem "implementation checkboxes belong in a separate implementation document: ${entry_name}/README.adoc"
  fi

  row_rfds+=("$entry_name")
  row_states+=("${state:-\(missing\)}")
  row_tasks+=("$task_summary")
  row_titles+=("$title")
  row_labels+=("${labels:-\(missing labels\)}")
  if [[ ${#title} -gt $title_width ]]; then
    title_width=${#title}
  fi
done

printf -v title_rule '%*s' "$title_width" ''
title_rule=${title_rule// /-}
printf '%-4s  %-13s  %5s  %-*s  %s\n' RFD State Tasks "$title_width" Title Labels
printf '%-4s  %-13s  %5s  %-*s  %s\n' ---- ------------- ----- "$title_width" "$title_rule" --------------------
for index in "${!row_rfds[@]}"; do
  printf '%-4s  %-13s  %5s  %-*s  %s\n' \
    "${row_rfds[$index]}" \
    "${row_states[$index]}" \
    "${row_tasks[$index]}" \
    "$title_width" \
    "${row_titles[$index]}" \
    "${row_labels[$index]}"
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
