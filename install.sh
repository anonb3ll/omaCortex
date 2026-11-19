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
    -h|--help) sed -n '2,13p' "$0"; exit 0 ;;
    *) die "unknown option: $arg" ;;
  esac
done

build() {
  say "Building omacortex with cargo (a minute or two the first time)..."
  (cd "$root" && cargo build --release --locked)
}

download() {
  version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)
  case "$(uname -m)" in
    x86_64|amd64) arch=x86_64 ;;
    aarch64|arm64) arch=aarch64 ;;
    *) die "no prebuilt binary for $(uname -m); install rust (sudo pacman -S rust) and run ./install.sh --build" ;;
  esac
  name="omacortex-$version-$arch-linux"
  base="https://github.com/$repo/releases/download/v$version"
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  say "Downloading $name.tar.gz from the v$version release..."
  curl -fsSL --proto '=https' -o "$tmp/$name.tar.gz" "$base/$name.tar.gz" || die "download failed ($base)"
  curl -fsSL --proto '=https' -o "$tmp/checksums.txt" "$base/checksums.txt" || die "checksums download failed"
  expected=$(grep " \./$name.tar.gz\$\| $name.tar.gz\$" "$tmp/checksums.txt" | cut -d' ' -f1)
  [ -n "$expected" ] || die "no checksum for $name.tar.gz"
  actual=$(sha256sum "$tmp/$name.tar.gz" | cut -d' ' -f1)
  [ "$expected" = "$actual" ] || die "checksum mismatch for $name.tar.gz"
  tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
  mkdir -p "$root/libexec"
  install -m755 "$tmp/$name/bin/omacortex" "$root/libexec/omacortex"
  say "Installed $root/libexec/omacortex (sha256 verified)"
}

case "$mode" in
  build) command -v cargo >/dev/null 2>&1 || die "cargo not found (sudo pacman -S rust)"; build ;;
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
command -v omarchy-shell >/dev/null 2>&1 && omarchy-shell shell reloadPlugins >/dev/null 2>&1 || true

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
