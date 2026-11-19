"""What is playing, from MPRIS on the session bus.

Uses ``busctl --user --json=short`` (part of systemd, so always present on
Omarchy) instead of a Python D-Bus binding, and polls: one cheap call per
player every few seconds.
"""

from __future__ import annotations

import json
import logging
import shutil
import subprocess
import threading
from collections.abc import Callable

from omacortex.recorder import Track

log = logging.getLogger(__name__)

PREFIX = "org.mpris.MediaPlayer2."
PATH = "/org/mpris/MediaPlayer2"
IFACE = "org.mpris.MediaPlayer2.Player"


def _busctl(*args: str, timeout: float = 3.0) -> str:
    out = subprocess.run(
        ["busctl", "--user", *args],
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )
    return out.stdout if out.returncode == 0 else ""


def list_players() -> list[str]:
    text = _busctl("--json=short", "list", "--acquired")
    names: list[str] = []
    try:
        for entry in json.loads(text or "[]"):
            name = entry.get("name", "") if isinstance(entry, dict) else ""
            if name.startswith(PREFIX):
                names.append(name)
    except ValueError:
        for line in text.splitlines():
            first = line.split(" ", 1)[0]
            if first.startswith(PREFIX):
                names.append(first)
    return sorted(set(names))


def player_label(bus_name: str) -> str:
    """org.mpris.MediaPlayer2.chromium.instance123 -> chromium"""
    return bus_name[len(PREFIX) :].split(".", 1)[0] if bus_name.startswith(PREFIX) else bus_name


def parse_properties(text: str) -> tuple[str, dict]:
    """Parse two JSON lines from `busctl get-property ... PlaybackStatus Metadata`."""
    lines = [ln for ln in text.splitlines() if ln.strip()]
    if len(lines) < 2:
        return "", {}
    try:
        status = json.loads(lines[0]).get("data", "")
        meta_raw = json.loads(lines[1]).get("data", {})
    except (ValueError, AttributeError):
        return "", {}
    meta = {}
    if isinstance(meta_raw, dict):
        for key, wrapped in meta_raw.items():
            if isinstance(wrapped, dict) and "data" in wrapped:
                meta[key] = wrapped["data"]
    return str(status), meta


def track_from(bus_name: str, status: str, meta: dict) -> Track | None:
    if status != "Playing":
        return None
    title = meta.get("xesam:title") or ""
    if not isinstance(title, str) or not title.strip():
        return None
    artist = meta.get("xesam:artist") or ""
    if isinstance(artist, list):
        artist = ", ".join(str(a) for a in artist[:3])
    url = meta.get("xesam:url") or ""
    # Local file paths stay out of media rows; the files sensor owns those.
    if not isinstance(url, str) or url.startswith("file:"):
        url = ""
    return Track(player=player_label(bus_name), title=title.strip(), artist=str(artist), url=url)


def playing() -> list[Track]:
    tracks = []
    for name in list_players():
        status, meta = parse_properties(
            _busctl(
                "--json=short",
                "get-property",
                name,
                PATH,
                IFACE,
                "PlaybackStatus",
                "Metadata",
            )
        )
        track = track_from(name, status, meta)
        if track:
            tracks.append(track)
    return tracks


class MediaCollector:
    def __init__(
        self,
        on_tracks: Callable[[list[Track]], None],
        stop: threading.Event,
        interval: float = 5.0,
    ) -> None:
        self.on_tracks = on_tracks
        self.stop = stop
        self.interval = max(1.0, interval)

    def run(self) -> None:
        if not shutil.which("busctl"):
            log.warning("media: busctl not found; media capture disabled")
            return
        while not self.stop.is_set():
            try:
                self.on_tracks(playing())
            except (OSError, subprocess.SubprocessError) as exc:
                log.warning("media: %s", exc)
            self.stop.wait(self.interval)
