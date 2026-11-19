"""Configuration and on-disk locations.

Everything lives under the XDG base directories of the current user:

- config: ``$XDG_CONFIG_HOME/omacortex/config.toml`` (optional)
- data:   ``$XDG_DATA_HOME/omacortex/cortex.db`` (the activity store)
- state:  ``$XDG_STATE_HOME/omacortex/`` (consent, pause, daemon lock, control socket)

The data and state directories are created with mode 0700 and the database
with mode 0600, so other local users cannot read them.
"""

from __future__ import annotations

import copy
import os
import tomllib
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

DEFAULTS: dict[str, Any] = {
    "capture": {
        # Focused window (app class + title) from Hyprland's event socket.
        "windows": True,
        # What is playing, from MPRIS (browsers, mpv, Spotify, ...).
        "media": True,
        # Files you open, from the freedesktop recently-used list.
        "files": True,
        # Focus spans shorter than this are dropped (alt-tab noise).
        "min_span_seconds": 2,
        # Seconds without input before the shell plugin reports you as away.
        "idle_seconds": 300,
        # How often to poll MPRIS players and the recent-files list.
        "media_poll_seconds": 5,
        "files_poll_seconds": 60,
    },
    "privacy": {
        # Activity older than this is deleted automatically. 0 keeps everything.
        "retention_days": 30,
        # Replace e-mail addresses, tokens and URL query strings in titles.
        "redact": True,
        # Window classes that are never recorded (case-insensitive, exact or glob).
        "exclude_apps": [
            "1password",
            "bitwarden",
            "org.keepassxc.keepassxc",
            "keepassxc",
            "proton-pass",
            "proton pass",
            "*pinentry*",
            "*polkit*",
            "org.gnome.seahorse.application",
            "gcr-prompter",
        ],
        # Window titles matching any of these regular expressions are never recorded.
        "exclude_titles": [
            r"(?i)\bincognito\b",
            r"(?i)private browsing",
            r"(?i)\bInPrivate\b",
            r"(?i)\bpassword\b",
            r"(?i)\b(sign|log) ?in\b",
            r"(?i)\bbank(ing)?\b",
            r"(?i)\b2fa\b|two-factor|authenticator",
        ],
        # Workspaces never recorded, by name (for example "special:private").
        "exclude_workspaces": [],
        # Files under these directories are never recorded.
        "exclude_paths": [
            "~/.ssh",
            "~/.gnupg",
            "~/.password-store",
            "~/.local/share/keyrings",
            "~/.config/omacortex",
        ],
    },
    "model": {
        # "auto" probes local OpenAI-compatible servers on this machine
        # (omarchy agent local, Ollama, llama.cpp, LM Studio). Or set a URL
        # such as "http://127.0.0.1:11434/v1". "off" disables the model.
        "endpoint": "auto",
        # Empty picks the first model the server lists.
        "model": "",
        # Refuse any endpoint that is not on this machine unless set to true.
        "allow_remote": False,
        "timeout_seconds": 120,
        "max_tokens": 600,
    },
    "mcp": {
        # Let agents add notes and checkpoints to your timeline.
        "allow_notes": True,
        # Hard cap on rows any single MCP call returns.
        "max_results": 200,
    },
}


def _xdg(var: str, fallback: str) -> Path:
    value = os.environ.get(var, "").strip()
    if value and os.path.isabs(value):
        return Path(value)
    return Path.home() / fallback


@dataclass(frozen=True)
class Paths:
    config_dir: Path
    data_dir: Path
    state_dir: Path

    @property
    def config_file(self) -> Path:
        return self.config_dir / "config.toml"

    @property
    def db(self) -> Path:
        return self.data_dir / "cortex.db"

    @property
    def consent_file(self) -> Path:
        return self.state_dir / "consent.json"

    @property
    def pause_file(self) -> Path:
        return self.state_dir / "pause.json"

    @property
    def lock_file(self) -> Path:
        return self.state_dir / "daemon.lock"

    @property
    def control_socket(self) -> Path:
        return self.state_dir / "control.sock"

    def ensure(self) -> None:
        for directory in (self.data_dir, self.state_dir):
            directory.mkdir(parents=True, exist_ok=True)
            os.chmod(directory, 0o700)


def default_paths() -> Paths:
    return Paths(
        config_dir=_xdg("XDG_CONFIG_HOME", ".config") / "omacortex",
        data_dir=_xdg("XDG_DATA_HOME", ".local/share") / "omacortex",
        state_dir=_xdg("XDG_STATE_HOME", ".local/state") / "omacortex",
    )


def _merge(base: dict[str, Any], override: dict[str, Any]) -> dict[str, Any]:
    out = copy.deepcopy(base)
    for key, value in override.items():
        if isinstance(value, dict) and isinstance(out.get(key), dict):
            out[key] = _merge(out[key], value)
        else:
            out[key] = value
    return out


@dataclass
class Config:
    paths: Paths
    data: dict[str, Any] = field(default_factory=lambda: copy.deepcopy(DEFAULTS))
    error: str = ""

    def get(self, section: str, key: str) -> Any:
        return self.data.get(section, {}).get(key, DEFAULTS[section][key])


def load(paths: Paths | None = None) -> Config:
    paths = paths or default_paths()
    cfg = Config(paths=paths)
    if paths.config_file.is_file():
        try:
            with paths.config_file.open("rb") as handle:
                cfg.data = _merge(DEFAULTS, tomllib.load(handle))
        except (OSError, tomllib.TOMLDecodeError) as exc:
            # A broken config must not silently widen capture: keep defaults
            # and surface the error in `omacortex status` and `doctor`.
            cfg.error = f"{paths.config_file}: {exc}"
    return cfg


EXAMPLE_CONFIG = """\
# omaCortex configuration. Every key is optional; these are the defaults.
# Changes apply when the daemon restarts: `omarchy-shell shell reloadPlugins`
# or `systemctl --user restart omacortex`.

[capture]
windows = true            # focused window class + title (Hyprland)
media = true              # what is playing (MPRIS)
files = true              # files you open (recently-used.xbel)
min_span_seconds = 2      # drop focus spans shorter than this
idle_seconds = 300        # away after this long without input

[privacy]
retention_days = 30       # 0 keeps everything
redact = true             # scrub e-mails, tokens and URL queries from titles
exclude_apps = ["1password", "bitwarden", "org.keepassxc.keepassxc", "keepassxc", "proton-pass", "proton pass", "*pinentry*", "*polkit*", "org.gnome.seahorse.application", "gcr-prompter"]
exclude_titles = ['(?i)\\bincognito\\b', '(?i)private browsing', '(?i)\\bInPrivate\\b', '(?i)\\bpassword\\b', '(?i)\\b(sign|log) ?in\\b', '(?i)\\bbank(ing)?\\b', '(?i)\\b2fa\\b|two-factor|authenticator']
exclude_workspaces = []   # e.g. ["special:private"]
exclude_paths = ["~/.ssh", "~/.gnupg", "~/.password-store", "~/.local/share/keyrings", "~/.config/omacortex"]

[model]
endpoint = "auto"         # "auto", "off", or e.g. "http://127.0.0.1:11434/v1"
model = ""                # empty = first model the server lists
allow_remote = false      # never send activity off this machine unless true

[mcp]
allow_notes = true        # agents may add notes/checkpoints
max_results = 200
"""
