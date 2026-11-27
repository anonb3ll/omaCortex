#!/usr/bin/env bash
# Run the same pinned rules on commit history and on an exported source tree.
set -euo pipefail

mode=${1:-}
case "$mode" in
  git)
    if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
      echo "usage: $0 git HEAD [BASE]" >&2
      exit 2
    fi
    source=$(git rev-parse --show-toplevel)
    if [ "$(git rev-parse --is-shallow-repository)" != false ]; then
      echo "Secret scan requires a complete Git checkout." >&2
      exit 2
    fi
    head=$(git rev-parse --verify --end-of-options "$2^{commit}")
    range=$head
    if [ -n "${3:-}" ] && [ "$3" != 0000000000000000000000000000000000000000 ]; then
      base=$(git rev-parse --verify --end-of-options "$3^{commit}")
      range="$base..$head"
    fi
    ;;
  dir)
    if [ "$#" -ne 2 ] || [ ! -d "$2" ]; then
      echo "usage: $0 dir DIRECTORY" >&2
      exit 2
    fi
    source=$(cd -- "$2" && pwd -P)
    ;;
  *)
    echo "usage: $0 {git HEAD [BASE]|dir DIRECTORY}" >&2
    exit 2
    ;;
esac

version=8.30.1
case "$(uname -s):$(uname -m)" in
  Linux:x86_64)
    platform=linux_x64
    sha256=551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb
    ;;
  Linux:aarch64)
    platform=linux_arm64
    sha256=e4a487ee7ccd7d3a7f7ec08657610aa3606637dab924210b3aee62570fb4b080
    ;;
  Darwin:x86_64)
    platform=darwin_x64
    sha256=dfe101a4db2255fc85120ac7f3d25e4342c3c20cf749f2c20a18081af1952709
    ;;
  Darwin:arm64)
    platform=darwin_arm64
    sha256=b40ab0ae55c505963e365f271a8d3846efbc170aa17f2607f13df610a9aeb6a5
    ;;
  *)
    echo "No pinned Gitleaks build for this platform." >&2
    exit 2
    ;;
esac

if [[ $platform == darwin_* ]]; then
  checksum=(shasum -a 256)
else
  checksum=(sha256sum)
fi
scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
archive="gitleaks_${version}_${platform}.tar.gz"
curl --proto '=https' --proto-redir '=https' --fail --silent --show-error --location \
  --connect-timeout 15 --max-time 90 --retry 2 \
  "https://github.com/gitleaks/gitleaks/releases/download/v${version}/$archive" -o "$scratch/$archive"
printf '%s  %s\n' "$sha256" "$scratch/$archive" | "${checksum[@]}" -c >/dev/null
tar -xzf "$scratch/$archive" -C "$scratch" gitleaks

# A change under review cannot disable built-in rules with repo config,
# .gitleaksignore or inline allow comments. CI never prints scanner output.
printf '[extend]\nuseDefault = true\n' > "$scratch/config.toml"
touch "$scratch/ignore"
scan() {
  "$scratch/gitleaks" "$@" --config "$scratch/config.toml" \
    --gitleaks-ignore-path "$scratch/ignore" --ignore-gitleaks-allow \
    --redact=100 --exit-code=42 --timeout=120 --no-banner >/dev/null 2>&1
}

# Prove the installed scanner still accepts a placeholder and rejects a
# synthetic provider token, even with an inline allow comment. The value is
# assembled at runtime and stays within the disposable fixture.
fixture="$scratch/fixture"
mkdir "$fixture"
printf '%s\n' 'GITHUB_TOKEN=replace-with-your-token' > "$fixture/.env.example"
if ! scan dir "$fixture"; then
  echo "Secret scanner failed its clean control." >&2
  exit 2
fi
printf 'GITHUB_TOKEN=%s_%s # %s\n' ghp 'aB9cD8eF7gH6iJ5kL4mN3pQ2rS1tU0vWxyZ9' \
  'gitleaks:allow' > "$fixture/.env"
result=0
scan dir "$fixture" || result=$?
if [ "$result" -ne 42 ]; then
  echo "Secret scanner failed its detection control." >&2
  exit 2
fi

result=0
if [ "$mode" = git ]; then
  scan git "$source" --log-opts="--full-history --diff-merges=first-parent --no-ext-diff --no-textconv $range" \
    || result=$?
else
  scan dir "$source" || result=$?
fi
case "$result" in
  0) echo "Secret scan passed (Gitleaks $version, $mode)." ;;
  42) echo "Secret scan found potential credentials. Scanner details are suppressed in CI." >&2 ;;
  *) echo "Secret scan could not complete. Scanner details are suppressed in CI." >&2 ;;
esac
exit "$result"
