#!/bin/sh
# omaCortex installer for use outside (or alongside) the Omarchy shell plugin.
#
#   ./install.sh            link `omacortex` into ~/.local/bin
#   ./install.sh --systemd  also run the recorder as a systemd user service
#                           (use this if you do not enable the shell plugin)
#   ./install.sh --uninstall
#
# It never grants consent: recording starts only after `omacortex enable`.
set -eu

root=$(cd "$(dirname "$0")" && pwd -P)
bin_dir="$HOME/.local/bin"
unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
unit="$unit_dir/omacortex.service"

say() { printf '%s\n' "$*"; }

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
  say "Your history is untouched. Delete it with: rm -rf ~/.local/share/omacortex ~/.local/state/omacortex"
}

case "${1:-}" in
  --uninstall) uninstall; exit 0 ;;
  --systemd|"") ;;
  -h|--help) sed -n '2,10p' "$0"; exit 0 ;;
  *) say "unknown option: $1"; exit 2 ;;
esac

mkdir -p "$bin_dir"
ln -sfn "$root/bin/omacortex" "$bin_dir/omacortex"
say "linked $bin_dir/omacortex"

if [ "${1:-}" = "--systemd" ]; then
  mkdir -p "$unit_dir"
  sed "s|@OMACORTEX@|$root/bin/omacortex|" "$root/systemd/omacortex.service" > "$unit"
  systemctl --user daemon-reload
  systemctl --user enable --now omacortex.service
  say "installed and started $unit"
fi

"$root/bin/omacortex" --version
cat <<EOF

Next:
  omacortex enable          start recording (nothing is recorded until you do)
  omacortex doctor          check sensors and your local model
  omacortex now             what you are doing right now

Connect an agent over MCP (stdio):
  claude mcp add omacortex -- $bin_dir/omacortex mcp
  codex mcp add omacortex -- $bin_dir/omacortex mcp
EOF
