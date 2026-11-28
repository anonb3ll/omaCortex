#!/usr/bin/env bash
# Proves the identity gate rejects a planted Gmail commit and ignores older history.
# Forbidden strings are assembled at runtime so this file stays clean.
set -euo pipefail

LC_ALL=C
export LC_ALL

root=$(cd "$(dirname "$0")/.." && pwd)
gate=$root/scripts/identity-gate.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

fail() {
  printf 'identity-gate-test: %s\n' "$1" >&2
  exit 1
}

full_name() {
  printf '%s %s' "$(printf '%s%s' Anth ony)" "$(printf '%s%s' Bell ino)"
}

gmail() {
  printf '%s@%s.%s' "$1" "$(printf '%s%s' gm ail)" "$(printf '%s%s' c om)"
}

handle() {
  printf '%s%s' ant mbel
}

new_repo() {
  local dir=$1
  git init -q "$dir"
  git -C "$dir" config user.name anonb3ll
  git -C "$dir" config user.email anonb3ll@pm.me
  git -C "$dir" config commit.gpgsign false
}

commit_as() {
  local dir=$1 name=$2 email=$3 message=$4
  GIT_AUTHOR_NAME=$name \
    GIT_AUTHOR_EMAIL=$email \
    GIT_COMMITTER_NAME=$name \
    GIT_COMMITTER_EMAIL=$email \
    git -C "$dir" commit -q --allow-empty -m "$message"
}

assert_rejected() {
  local label=$1 output=$2 code=$3 sha=$4 secret=$5
  if [ "$code" -eq 0 ]; then
    fail "$label was accepted"
  fi
  if ! printf '%s\n' "$output" | grep -q "$sha"; then
    fail "$label did not print the offending SHA"
  fi
  if printf '%s\n' "$output" | grep -qF -- "$secret"; then
    fail "$label printed a forbidden address"
  fi
}

# A planted Gmail author is rejected, and the report names the SHA only.
repo=$work/planted
new_repo "$repo"
printf 'ok\n' >"$repo/README"
git -C "$repo" add README
git -C "$repo" commit -q -m 'base'
base=$(git -C "$repo" rev-parse HEAD)
secret=$(gmail planted-gate)
commit_as "$repo" anonb3ll "$secret" 'planted author'
sha=$(git -C "$repo" rev-parse HEAD)
set +e
output=$("$gate" commits "$base" "$sha" "$repo" 2>&1)
code=$?
set -e
assert_rejected 'planted Gmail author' "$output" "$code" "$sha" "$secret"
printf '%s\n' "$output" | grep -q 'forbidden gmail address' || fail 'planted Gmail author had no category'

# The same address in a Co-authored-by trailer is rejected.
repo=$work/trailer
new_repo "$repo"
git -C "$repo" commit -q --allow-empty -m 'base'
base=$(git -C "$repo" rev-parse HEAD)
secret=$(gmail planted-trailer)
git -C "$repo" commit -q --allow-empty -m "note" -m "Co-authored-by: Someone <$secret>"
sha=$(git -C "$repo" rev-parse HEAD)
set +e
output=$("$gate" commits "$base" "$sha" "$repo" 2>&1)
code=$?
set -e
assert_rejected 'planted Gmail trailer' "$output" "$code" "$sha" "$secret"

# A Gmail commit that is the base, not part of base..head, is left alone.
repo=$work/history
new_repo "$repo"
secret=$(gmail old-history)
commit_as "$repo" anonb3ll "$secret" 'old'
old=$(git -C "$repo" rev-parse HEAD)
commit_as "$repo" anonb3ll anonb3ll@pm.me 'new'
head=$(git -C "$repo" rev-parse HEAD)
set +e
output=$("$gate" commits "$old" "$head" "$repo" 2>&1)
code=$?
set -e
if [ "$code" -ne 0 ]; then
  fail "older history was scanned"
fi
if printf '%s\n' "$output" | grep -q "$old"; then
  fail "older SHA was reported"
fi

# Allowed GitHub accounts pass, including as committer and as a trailer.
repo=$work/allowed
new_repo "$repo"
git -C "$repo" commit -q --allow-empty -m 'base'
base=$(git -C "$repo" rev-parse HEAD)
GIT_AUTHOR_NAME='web-flow' \
  GIT_AUTHOR_EMAIL='1083313+web-flow@users.noreply.github.com' \
  GIT_COMMITTER_NAME='github-actions[bot]' \
  GIT_COMMITTER_EMAIL='41898282+github-actions[bot]@users.noreply.github.com' \
  git -C "$repo" commit -q --allow-empty \
  -m 'bots' \
  -m 'Co-authored-by: dependabot[bot] <49699333+dependabot[bot]@users.noreply.github.com>'
head=$(git -C "$repo" rev-parse HEAD)
"$gate" commits "$base" "$head" "$repo"

# The operator's own identity is accepted in COMMIT METADATA: a merge commit
# GitHub creates is authored with it, so a PR landing must not be refused for
# that. Every other Gmail address above is still refused.
repo=$work/operator
new_repo "$repo"
git -C "$repo" commit -q --allow-empty -m 'base'
base=$(git -C "$repo" rev-parse HEAD)
commit_as "$repo" 'Operator' "$(gmail "$(handle)")" 'Merge pull request #1 from anonb3ll/topic'
head=$(git -C "$repo" rev-parse HEAD)
if ! "$gate" commits "$base" "$head" "$repo"; then
  fail 'operator identity in commit metadata was rejected'
fi

# The exemption is exact, so a look-alike that merely shares the handle, or a
# +tag variant of the mailbox, is still refused. Without these two cases a
# substring or prefix match would pass the suite.
repo=$work/operator-lookalike
new_repo "$repo"
git -C "$repo" commit -q --allow-empty -m 'base'
base=$(git -C "$repo" rev-parse HEAD)
secret=$(gmail "not-$(handle)")
commit_as "$repo" 'Operator' "$secret" 'look-alike author'
sha=$(git -C "$repo" rev-parse HEAD)
set +e
output=$("$gate" commits "$base" "$sha" "$repo" 2>&1)
code=$?
set -e
if [ "$code" -eq 0 ]; then
  fail 'handle-prefix look-alike was accepted'
fi

repo=$work/operator-plustag
new_repo "$repo"
git -C "$repo" commit -q --allow-empty -m 'base'
base=$(git -C "$repo" rev-parse HEAD)
secret=$(gmail "$(handle)+work")
commit_as "$repo" 'Operator' "$secret" '+tag author'
sha=$(git -C "$repo" rev-parse HEAD)
set +e
output=$("$gate" commits "$base" "$sha" "$repo" 2>&1)
code=$?
set -e
if [ "$code" -eq 0 ]; then
  fail '+tag variant was accepted'
fi

# The trailer keeps refusing the operator address: a merge never adds one, so a
# trailer claiming it is a fabricated co-author rather than a landing artifact.
repo=$work/operator-trailer
new_repo "$repo"
git -C "$repo" commit -q --allow-empty -m 'base'
base=$(git -C "$repo" rev-parse HEAD)
secret=$(gmail "$(handle)")
git -C "$repo" commit -q --allow-empty -m 'merge' -m "Co-authored-by: Operator <$secret>"
sha=$(git -C "$repo" rev-parse HEAD)
set +e
output=$("$gate" commits "$base" "$sha" "$repo" 2>&1)
code=$?
set -e
if [ "$code" -eq 0 ]; then
  fail 'operator address in a Co-authored-by trailer was accepted'
fi

# The same identity is accepted as the committer as well, and the author may be
# someone else - the allowance is per field, not per commit.
repo=$work/operator-committer
new_repo "$repo"
git -C "$repo" commit -q --allow-empty -m 'base'
base=$(git -C "$repo" rev-parse HEAD)
GIT_AUTHOR_NAME='anonb3ll' \
  GIT_AUTHOR_EMAIL='anonb3ll@pm.me' \
  GIT_COMMITTER_NAME='Operator' \
  GIT_COMMITTER_EMAIL="$(gmail "$(handle)")" \
  git -C "$repo" commit -q --allow-empty -m 'committed by the operator'
head=$(git -C "$repo" rev-parse HEAD)
"$gate" commits "$base" "$head" "$repo" || fail 'operator committer was rejected'


repo=$work/name
new_repo "$repo"
git -C "$repo" commit -q --allow-empty -m 'base'
base=$(git -C "$repo" rev-parse HEAD)
commit_as "$repo" "$(full_name)" '1+anonb3ll@users.noreply.github.com' 'named'
sha=$(git -C "$repo" rev-parse HEAD)
set +e
output=$("$gate" commits "$base" "$sha" "$repo" 2>&1)
code=$?
set -e
if [ "$code" -eq 0 ]; then
  fail 'retired name was accepted'
fi
printf '%s\n' "$output" | grep -q "$sha" || fail 'retired name did not print the SHA'
printf '%s\n' "$output" | grep -q 'forbidden full name' || fail 'retired name had no category'

# Tracked files: a clean tree passes, and planted file text fails without echoing it.
repo=$work/files
new_repo "$repo"
printf 'clean\n' >"$repo/README"
git -C "$repo" add README
git -C "$repo" commit -q -m 'clean files'
"$gate" files "$repo"

secret=$(gmail in-file)
printf 'see %s\n' "$secret" >"$repo/note.txt"
git -C "$repo" add note.txt
git -C "$repo" commit -q -m 'bad file'
set +e
output=$("$gate" files "$repo" 2>&1)
code=$?
set -e
if [ "$code" -eq 0 ]; then
  fail 'file address was accepted'
fi
printf '%s\n' "$output" | grep -q 'note.txt' || fail 'file address did not name the file'
if printf '%s\n' "$output" | grep -qF -- "$secret"; then
  fail 'file report printed a forbidden address'
fi

printf '%s\n' "$(handle)" >"$repo/handle.txt"
git -C "$repo" add handle.txt
git -C "$repo" commit -q -m 'bad handle'
set +e
output=$("$gate" files "$repo" 2>&1)
code=$?
set -e
if [ "$code" -eq 0 ]; then
  fail 'file handle was accepted'
fi

hyphen=$(full_name | tr ' ' '-')
printf '%s\n' "$hyphen" >"$repo/hyphen.txt"
git -C "$repo" add hyphen.txt
git -C "$repo" commit -q -m 'bad hyphen'
set +e
output=$("$gate" files "$repo" 2>&1)
code=$?
set -e
if [ "$code" -eq 0 ]; then
  fail 'separated name in a file was accepted'
fi

# The operator address is STILL a finding in tracked file content. The ruling
# covers commit metadata; `files` mode did not change with it.
repo=$work/operator-file
new_repo "$repo"
printf 'clean\n' >"$repo/README"
git -C "$repo" add README
git -C "$repo" commit -q -m 'clean'
secret=$(gmail "$(handle)")
printf 'see %s\n' "$secret" >"$repo/operator.txt"
git -C "$repo" add operator.txt
git -C "$repo" commit -q -m 'operator address in a file'
set +e
output=$("$gate" files "$repo" 2>&1)
code=$?
set -e
if [ "$code" -eq 0 ]; then
  fail 'operator address in a file was accepted'
fi
printf '%s\n' "$output" | grep -q 'operator.txt' || fail 'operator file address did not name the file'
if printf '%s\n' "$output" | grep -qF -- "$secret"; then
  fail 'operator file report printed the address'
fi

printf 'identity-gate-test: ok\n'
