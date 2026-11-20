#!/bin/sh
# Resolve which published release a fresh install should download.
#
#   scripts/fresh-install-version.sh <event> <cargo-version> <latest-tag> <tag-exists> <ref>
#
# tag-exists is yes or no: whether refs/tags/v<cargo-version> exists.
# Prints the version to download.
# A pull request whose version tag does not exist yet gets the latest release.
# Every other event, and any refs/tags/* ref, requires latest-tag to be
# v<cargo-version> and prints the checkout version.
set -eu

if [ "$#" -ne 5 ]; then
  printf 'usage: fresh-install-version.sh <event> <cargo-version> <latest-tag> <tag-exists> <ref>\n' >&2
  exit 2
fi

event=$1
version=$2
latest=$3
tag_exists=$4
ref=$5

case "$tag_exists" in
  yes|no) ;;
  *)
    printf 'fresh-install-version: tag-exists must be yes or no\n' >&2
    exit 2
    ;;
esac

if [ -z "$version" ] || [ -z "$latest" ] || [ "$latest" = "null" ]; then
  printf 'fresh-install-version: missing version or latest release\n' >&2
  exit 1
fi

strict=yes
if [ "$event" = "pull_request" ] && [ "$tag_exists" = "no" ]; then
  strict=no
fi
case "$ref" in
  refs/tags/*) strict=yes ;;
esac

if [ "$strict" = "yes" ]; then
  if [ "$latest" != "v${version}" ]; then
    printf 'fresh-install: latest release %s does not match v%s\n' "$latest" "$version" >&2
    exit 1
  fi
  printf '%s\n' "$version"
  exit 0
fi

case "$latest" in
  v*) printf '%s\n' "${latest#v}" ;;
  *)
    printf 'fresh-install: latest release tag %s has no v prefix\n' "$latest" >&2
    exit 1
    ;;
esac
