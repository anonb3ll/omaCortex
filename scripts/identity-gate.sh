#!/usr/bin/env bash
# Block a retired personal name, a retired handle, and Gmail addresses.
#
#   scripts/identity-gate.sh files [repo]
#   scripts/identity-gate.sh commits <base> <head> [repo]
#
# `commits` inspects base..head only. The base commit itself is not inspected.
# GitHub noreply addresses, the web-flow account, and bot accounts are allowed.
# Findings name the commit SHA or file and the kind of match. They never
# include the matched address.
set -euo pipefail

LC_ALL=C
export LC_ALL

usage() {
  printf 'usage: identity-gate.sh files [repo]\n' >&2
  printf '       identity-gate.sh commits <base> <head> [repo]\n' >&2
  exit 2
}

# Pieces are joined at runtime so this file does not contain the forbidden text.
name_re() {
  local first last
  first=$(printf '%s%s' anth ony)
  last=$(printf '%s%s' bell ino)
  printf '%s[^[:alnum:]]*%s|%s[^[:alnum:]]*%s' "$first" "$last" "$last" "$first"
}

handle_re() {
  printf '%s%s' ant mbel
}

mail_re() {
  # Escaped dot so this is a pattern, not a literal address.
  printf '@%s\\.%s' "$(printf '%s%s' gm ail)" "$(printf '%s%s' c om)"
}

is_allowed() {
  local value=$1
  local lower
  lower=$(printf '%s' "$value" | tr '[:upper:]' '[:lower:]')
  case "$lower" in
    *'@users.noreply.github.com' | *'@noreply.github.com' | 'noreply@github.com')
      return 0
      ;;
    *'[bot]'*)
      return 0
      ;;
    'web-flow' | *'<web-flow@'* | 'web-flow@'*)
      return 0
      ;;
  esac
  # "123456+web-flow@users.noreply.github.com" is already covered above.
  # A bare web-flow login, with or without a GitHub id prefix, is the web UI.
  case "$lower" in
    'web-flow' | *'+web-flow' | *'+web-flow@'*)
      return 0
      ;;
  esac
  return 1
}

# Print a category name when the value is forbidden. Allowed identities print nothing.
classify() {
  local value=$1
  if is_allowed "$value"; then
    return 1
  fi
  if printf '%s' "$value" | grep -qiE "$(name_re)"; then
    printf '%s\n' 'forbidden full name'
    return 0
  fi
  if printf '%s' "$value" | grep -qiE "$(handle_re)"; then
    printf '%s\n' 'forbidden handle'
    return 0
  fi
  if printf '%s' "$value" | grep -qiE "$(mail_re)"; then
    printf '%s\n' 'forbidden @gmail.com address'
    return 0
  fi
  return 1
}

check_files() {
  local repo=$1
  local hits=0
  local line path lineno kind
  # git grep exits 1 when nothing matches.
  set +e
  line=$(git -C "$repo" grep -a -n -E -i \
    -e "$(name_re)" \
    -e "$(handle_re)" \
    -e "$(mail_re)" \
    HEAD --)
  local status=$?
  set -e
  if [ "$status" -eq 1 ]; then
    return 0
  fi
  if [ "$status" -ne 0 ]; then
    printf 'identity-gate: git grep failed\n' >&2
    return 2
  fi
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    # HEAD:path:lineno:text. Drop the text so a match is never echoed.
    path=${line#HEAD:}
    lineno=${path#*:}
    lineno=${lineno%%:*}
    path=${path%%:*}
    kind=$(classify "$line" || true)
    if [ -z "$kind" ]; then
      # The line matched a pattern but classify allowed it (noreply, bot, web-flow).
      # Re-classify the raw line only when it is not an allowed identity.
      # File contents are not account identities, so a match here is real
      # unless classify refused it because the whole line looked allowed.
      if is_allowed "$line"; then
        continue
      fi
      kind='forbidden pattern'
    fi
    printf 'identity-gate: %s:%s %s\n' "$path" "$lineno" "$kind"
    hits=1
  done <<<"$line"
  return "$hits"
}

trailer_values() {
  local body=$1
  printf '%s\n' "$body" | grep -i '^Co-authored-by:' || true
}

check_commits() {
  local base=$1 head=$2 repo=$3
  local hits=0 sha name email cname cemail body value kind
  if ! git -C "$repo" cat-file -e "${base}^{commit}" 2>/dev/null; then
    printf 'identity-gate: base commit is not in this repository\n' >&2
    return 2
  fi
  if ! git -C "$repo" cat-file -e "${head}^{commit}" 2>/dev/null; then
    printf 'identity-gate: head commit is not in this repository\n' >&2
    return 2
  fi
  while IFS= read -r sha; do
    [ -n "$sha" ] || continue
    name=$(git -C "$repo" log -1 --format='%an' "$sha")
    email=$(git -C "$repo" log -1 --format='%ae' "$sha")
    cname=$(git -C "$repo" log -1 --format='%cn' "$sha")
    cemail=$(git -C "$repo" log -1 --format='%ce' "$sha")
    body=$(git -C "$repo" log -1 --format='%b' "$sha")
    for value in "$name" "$email" "$cname" "$cemail"; do
      if kind=$(classify "$value"); then
        printf 'identity-gate: %s %s\n' "$sha" "$kind"
        hits=1
      fi
    done
    while IFS= read -r value; do
      [ -n "$value" ] || continue
      if kind=$(classify "$value"); then
        printf 'identity-gate: %s Co-authored-by %s\n' "$sha" "$kind"
        hits=1
      fi
    done < <(trailer_values "$body")
  done < <(git -C "$repo" rev-list "${base}..${head}")
  return "$hits"
}

[ "$#" -ge 1 ] || usage
mode=$1
shift
case "$mode" in
  files)
    repo=${1:-.}
    check_files "$repo"
    ;;
  commits)
    [ "$#" -ge 2 ] || usage
    base=$1
    head=$2
    repo=${3:-.}
    check_commits "$base" "$head" "$repo"
    ;;
  *)
    usage
    ;;
esac
