#!/bin/sh
# Installs the omaCortex recorder binary for the shell plugin and your shell.
#
#   ./install.sh               build with cargo if present, else download the
#                              matching release (checksum-verified); link
#                              `omacortex` into ~/.local/bin
#   ./install.sh --build       require a local cargo build
#   ./install.sh --download    require the prebuilt release binary
#   ./install.sh --systemd     also run the recorder as a systemd user service
#                              (only if you do not enable the shell plugin)
#   ./install.sh --uninstall
#
# Building needs Rust 1.89 or newer and a C compiler (cc, gcc, or clang).
# It never grants consent: recording starts only after `omacortex enable`.
set -eu

root=$(cd "$(dirname "$0")" && pwd -P)
repo="anonb3ll/omaCortex"
bin_dir="$HOME/.local/bin"
unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
unit="$unit_dir/omacortex.service"
mode=auto
systemd=no

say() { printf '%s\n' "$*"; }
die() { printf 'install.sh: %s\n' "$*" >&2; exit 1; }

uninstall() {
  if [ -f "$unit" ]; then
    systemctl --user disable --now omacortex.service 2>/dev/null || true
    rm -f "$unit"
    systemctl --user daemon-reload 2>/dev/null || true
    say "removed $unit"
  fi
  if [ -L "$bin_dir/omacortex" ]; then
    rm -f "$bin_dir/omacortex"
    say "removed $bin_dir/omacortex"
  fi
  rm -rf "$root/libexec"
  say "Your history is untouched. Delete it with: rm -rf ~/.local/share/omacortex ~/.local/state/omacortex"
}

for arg in "$@"; do
  case "$arg" in
    --uninstall) uninstall; exit 0 ;;
    --build) mode=build ;;
    --download) mode=download ;;
    --systemd) systemd=yes ;;
    -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
    *) die "unknown option: $arg" ;;
  esac
done

tool_advice() {
  cat <<EOF
Install Rust 1.89 or newer (https://rustup.rs) and a C compiler. Examples:
  sudo pacman -S rust base-devel
  sudo apt install rustc cargo build-essential
  sudo dnf install rust cargo gcc
Or skip the build and use the release binary: ./install.sh --download
EOF
}

require_build_tools() {
  if ! command -v rustc >/dev/null 2>&1; then
    printf 'install.sh: rustc was not found. Building needs Rust 1.89 or newer.\n' >&2
    tool_advice >&2
    exit 1
  fi
  ver=$(rustc --version | sed -n 's/^rustc \([0-9][0-9]*\)\.\([0-9][0-9]*\).*/\1 \2/p' | head -n 1)
  if [ -z "$ver" ]; then
    printf 'install.sh: could not read the rustc version. Building needs Rust 1.89 or newer.\n' >&2
    tool_advice >&2
    exit 1
  fi
  major=${ver%% *}
  minor=${ver#* }
  too_old=0
  if [ "$major" -lt 1 ]; then
    too_old=1
  elif [ "$major" -eq 1 ] && [ "$minor" -lt 89 ]; then
    too_old=1
  fi
  if [ "$too_old" -eq 1 ]; then
    printf 'install.sh: rustc %s.%s is too old. Building needs Rust 1.89 or newer.\n' "$major" "$minor" >&2
    tool_advice >&2
    exit 1
  fi
  compiler=${CC:-}
  if [ -n "$compiler" ]; then
    if ! command -v "$compiler" >/dev/null 2>&1; then
      printf 'install.sh: CC=%s was not found.\n' "$compiler" >&2
      tool_advice >&2
      exit 1
    fi
  elif ! command -v cc >/dev/null 2>&1 && ! command -v gcc >/dev/null 2>&1 && ! command -v clang >/dev/null 2>&1; then
    printf 'install.sh: no C compiler found (cc, gcc, or clang).\n' >&2
    printf 'install.sh: the build compiles the bundled SQLite sources.\n' >&2
    tool_advice >&2
    exit 1
  fi
}

build() {
  require_build_tools
  say "Building omacortex with cargo (a minute or two the first time)..."
  (cd "$root" && cargo build --release --locked)
}

# Print a release version without its leading v, or fail. Only
# MAJOR.MINOR.PATCH with an optional -PRERELEASE (dot-separated
# [0-9A-Za-z-] identifiers) and an optional leading v is accepted, so a
# version can never carry a path, a URL or shell syntax into a download.
release_version() {
  v=$1
  [ "${#v}" -le 64 ] || return 1
  case "$v" in
    '' | *[!0-9A-Za-z.-]*) return 1 ;;
  esac
  # The case above already refused newlines, so grep sees one line.
  printf '%s\n' "$v" |
    LC_ALL=C grep -Eq '^v?[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$' ||
    return 1
  printf '%s\n' "${v#v}"
}

# Print the URL the release assets are fetched from, or fail.
# OMACORTEX_RELEASE_BASE exists for the install test fixture only: it must be
# https on a loopback host, with plain path segments and no . or .. segment.
release_base() {
  if [ -z "${OMACORTEX_RELEASE_BASE+set}" ]; then
    printf 'https://github.com/%s/releases/download/v%s\n' "$repo" "$version"
    return 0
  fi
  b=$OMACORTEX_RELEASE_BASE
  [ "${#b}" -le 256 ] || return 1
  case "$b" in
    *[!0-9A-Za-z./:_~-]*) return 1 ;;
    https://*) ;;
    *) return 1 ;;
  esac
  rest=${b#https://}
  case "$rest/" in
    */./* | */../*) return 1 ;;
  esac
  printf '%s\n' "$rest" |
    LC_ALL=C grep -Eq '^(127\.0\.0\.1|localhost)(:[0-9]{1,5})?(/[0-9A-Za-z._~-]+)*$' ||
    return 1
  printf '%s\n' "$b"
}

download() {
  if [ -n "${OMACORTEX_INSTALL_VERSION+set}" ]; then
    version=$(release_version "$OMACORTEX_INSTALL_VERSION") ||
      die "OMACORTEX_INSTALL_VERSION must be a release version such as 1.2.3 or v1.2.3"
  else
    version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)
    version=$(release_version "$version") ||
      die "could not read a release version from Cargo.toml"
  fi
  case "$(uname -m)" in
    x86_64|amd64) arch=x86_64 ;;
    aarch64|arm64) arch=aarch64 ;;
    *) die "no prebuilt binary for $(uname -m); install Rust 1.89 or newer and run ./install.sh --build" ;;
  esac
  name="omacortex-$version-$arch-linux"
  base=$(release_base) ||
    die "OMACORTEX_RELEASE_BASE must be an https URL on 127.0.0.1 or localhost"
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  say "Downloading $name.tar.gz from the v$version release..."
  curl -fsSL --proto '=https' -o "$tmp/$name.tar.gz" "$base/$name.tar.gz" || die "download failed ($base)"
  curl -fsSL --proto '=https' -o "$tmp/checksums.txt" "$base/checksums.txt" || die "checksums download failed"
  # Exactly one line must name this archive, and its sum must be 64 hex
  # digits. A missing, duplicated or malformed entry stops the install.
  expected=$(awk -v n="$name.tar.gz" '$2 == n || $2 == "./" n { print $1 }' "$tmp/checksums.txt")
  case "$expected" in
    '' | *[!0-9a-f]*) die "no single sha256 checksum for $name.tar.gz" ;;
  esac
  [ "${#expected}" -eq 64 ] || die "no single sha256 checksum for $name.tar.gz"
  actual=$(sha256sum "$tmp/$name.tar.gz" | cut -d' ' -f1)
  [ "$expected" = "$actual" ] || die "checksum mismatch for $name.tar.gz"
  tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
  mkdir -p "$root/libexec"
  install -m755 "$tmp/$name/bin/omacortex" "$root/libexec/omacortex"
  say "Installed $root/libexec/omacortex (sha256 verified)"
}

case "$mode" in
  build)
    if ! command -v cargo >/dev/null 2>&1; then
      printf 'install.sh: cargo was not found.\n' >&2
      tool_advice >&2
      exit 1
    fi
    build
    ;;
  download) download ;;
  auto) if command -v cargo >/dev/null 2>&1; then build; else download; fi ;;
esac

mkdir -p "$bin_dir"
ln -sfn "$root/bin/omacortex" "$bin_dir/omacortex"
say "linked $bin_dir/omacortex"

if [ "$systemd" = yes ]; then
  mkdir -p "$unit_dir"
  sed "s|@OMACORTEX@|$root/bin/omacortex|" "$root/systemd/omacortex.service" > "$unit"
  systemctl --user daemon-reload
  systemctl --user enable --now omacortex.service
  say "installed and started $unit"
fi

# If the shell plugin is loaded, let it start the freshly installed recorder.
if command -v omarchy-shell >/dev/null 2>&1; then
  omarchy-shell shell reloadPlugins >/dev/null 2>&1 || true
fi

"$root/bin/omacortex" --version
cat <<EOF

Next:
  omacortex enable          start recording (nothing is recorded until you do)
  omacortex browser install remember the pages you read and watch (optional)
  omacortex doctor          check sensors and your local model
  omacortex now             what you are doing right now

Connect an agent over MCP (stdio):
  claude mcp add omacortex -- $bin_dir/omacortex mcp
  codex mcp add omacortex -- $bin_dir/omacortex mcp
EOF
