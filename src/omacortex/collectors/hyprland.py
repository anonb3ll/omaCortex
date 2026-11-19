"""Focused-window sensor over Hyprland's IPC sockets.

Reads ``.socket2.sock`` (the event stream) and asks ``.socket.sock`` for the
active window once at start and after each reconnect. Only the window class,
title and workspace name are used; nothing is read from the window itself.
"""

from __future__ import annotations

import json
import logging
import os
import socket
import threading
from collections.abc import Callable
from pathlib import Path

from omacortex.recorder import Window

log = logging.getLogger(__name__)


def socket_dir() -> Path | None:
    sig = os.environ.get("HYPRLAND_INSTANCE_SIGNATURE", "")
    runtime = os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")
    if sig:
        base = Path(runtime) / "hypr"
        if (base / sig / ".socket2.sock").exists():
            return base / sig
    # Not inherited (for example started from a systemd user unit before
    # Hyprland exported its environment): use the newest live instance.
    base = Path(runtime) / "hypr"
    candidates = (
        sorted(
            (p for p in base.glob("*/.socket2.sock")),
            key=lambda p: p.stat().st_mtime,
            reverse=True,
        )
        if base.is_dir()
        else []
    )
    return candidates[0].parent if candidates else None


def request(directory: Path, command: str, timeout: float = 2.0) -> str:
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        sock.settimeout(timeout)
        sock.connect(str(directory / ".socket.sock"))
        sock.sendall(command.encode())
        chunks = []
        while True:
            chunk = sock.recv(65536)
            if not chunk:
                break
            chunks.append(chunk)
            if sum(len(c) for c in chunks) > 4 * 1024 * 1024:
                break
    return b"".join(chunks).decode("utf-8", "replace")


def active_window(directory: Path) -> Window | None:
    try:
        data = json.loads(request(directory, "j/activewindow") or "{}")
    except (OSError, ValueError):
        return None
    if not isinstance(data, dict) or not data.get("class") and not data.get("title"):
        return None
    workspace = data.get("workspace") or {}
    return Window(
        app=str(data.get("class") or ""),
        title=str(data.get("title") or ""),
        workspace=str(workspace.get("name") or "") if isinstance(workspace, dict) else "",
    )


class EventParser:
    """Folds Hyprland events into "the focused window is now X" readings.

    Events used:
      activewindow>>CLASS,TITLE        focus moved (CLASS never contains a comma)
      activewindowv2>>ADDRESS          address of the newly focused window
      windowtitlev2>>ADDRESS,TITLE     a window's title changed
      workspacev2>>ID,NAME             the active workspace changed
      activespecial>>NAME,MONITOR      a special workspace was shown or hidden
      closewindow>>ADDRESS             a window closed
    """

    def __init__(self) -> None:
        self.address = ""
        self.window: Window | None = None
        self.workspace = ""
        self.special = ""

    def seed(self, window: Window | None) -> None:
        self.window = window
        if window:
            self.workspace = window.workspace

    def feed(self, line: str) -> tuple[bool, Window | None]:
        """Returns (changed, window)."""
        name, sep, data = line.rstrip("\n").partition(">>")
        if not sep:
            return False, self.window
        if name == "activewindow":
            app, _, title = data.partition(",")
            if not app and not title:
                self.window = None
            else:
                self.window = Window(app=app, title=title, workspace=self._ws())
            return True, self.window
        if name == "activewindowv2":
            self.address = data.strip()
            return False, self.window
        if name == "windowtitlev2":
            address, _, title = data.partition(",")
            if self.window and address == self.address and title != self.window.title:
                self.window = Window(app=self.window.app, title=title, workspace=self.window.workspace)
                return True, self.window
            return False, self.window
        if name == "workspacev2":
            _, _, ws_name = data.partition(",")
            self.workspace = ws_name
            return False, self.window
        if name == "activespecial":
            special, _, _ = data.partition(",")
            self.special = special
            return False, self.window
        if name == "closewindow":
            if data.strip() == self.address:
                self.address = ""
            return False, self.window
        return False, self.window

    def _ws(self) -> str:
        return self.special or self.workspace


class HyprlandCollector:
    def __init__(self, on_window: Callable[[Window | None], None], stop: threading.Event) -> None:
        self.on_window = on_window
        self.stop = stop

    def run(self) -> None:
        backoff = 1.0
        while not self.stop.is_set():
            directory = socket_dir()
            if directory is None:
                log.info("hyprland: no running instance found; retrying")
                self.stop.wait(min(backoff, 30))
                backoff = min(backoff * 2, 30)
                continue
            try:
                self._stream(directory)
                backoff = 1.0
            except OSError as exc:
                log.warning("hyprland: event socket error: %s", exc)
            self.stop.wait(min(backoff, 30))
            backoff = min(backoff * 2, 30)

    def _stream(self, directory: Path) -> None:
        parser = EventParser()
        parser.seed(active_window(directory))
        self.on_window(parser.window)
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
            sock.connect(str(directory / ".socket2.sock"))
            sock.settimeout(1.0)
            buffer = b""
            while not self.stop.is_set():
                try:
                    chunk = sock.recv(65536)
                except TimeoutError:
                    continue
                if not chunk:
                    raise OSError("event socket closed")
                buffer += chunk
                while b"\n" in buffer:
                    raw, buffer = buffer.split(b"\n", 1)
                    changed, window = parser.feed(raw.decode("utf-8", "replace"))
                    if changed:
                        self.on_window(window)
                if len(buffer) > 1024 * 1024:
                    buffer = b""
