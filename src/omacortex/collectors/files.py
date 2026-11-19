"""Files you open, from the freedesktop recently-used list.

GTK and Qt apps (and many others) already append every opened file to
``~/.local/share/recently-used.xbel``. omaCortex only reads that list; it
never watches the filesystem.
"""

from __future__ import annotations

import logging
import os
import threading
import xml.etree.ElementTree as ET
from collections.abc import Callable
from datetime import datetime
from pathlib import Path
from urllib.parse import unquote, urlsplit

log = logging.getLogger(__name__)

NS = {"bookmark": "http://www.freedesktop.org/standards/desktop-bookmarks"}
MAX_BYTES = 8 * 1024 * 1024


def xbel_path() -> Path:
    base = os.environ.get("XDG_DATA_HOME") or str(Path.home() / ".local/share")
    return Path(base) / "recently-used.xbel"


def _ts(value: str | None) -> float | None:
    if not value:
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp()
    except ValueError:
        return None


def parse_xbel(text: str) -> list[tuple[float, str, str]]:
    """Return (timestamp, path, app) for each local file bookmark."""
    try:
        # The user's own size-capped file; expat >= 2.4 bounds entity expansion.
        root = ET.fromstring(text)  # noqa: S314
    except ET.ParseError:
        return []
    out = []
    for bm in root.iter("bookmark"):
        href = bm.get("href", "")
        parts = urlsplit(href)
        if parts.scheme != "file":
            continue
        path = unquote(parts.path)
        best_ts = None
        best_app = ""
        for app in bm.iter(f"{{{NS['bookmark']}}}application"):
            ts = _ts(app.get("modified"))
            if ts is not None and (best_ts is None or ts > best_ts):
                best_ts, best_app = ts, app.get("name", "")
        if best_ts is None:
            best_ts = _ts(bm.get("visited")) or _ts(bm.get("modified")) or _ts(bm.get("added"))
        if best_ts is not None:
            out.append((best_ts, path, best_app))
    out.sort()
    return out


class FilesCollector:
    def __init__(
        self,
        on_file: Callable[[str, str, float], None],
        watermark: Callable[[], float],
        set_watermark: Callable[[float], None],
        stop: threading.Event,
        interval: float = 60.0,
        path: Path | None = None,
    ) -> None:
        self.on_file = on_file
        self.watermark = watermark
        self.set_watermark = set_watermark
        self.stop = stop
        self.interval = max(5.0, interval)
        self.path = path or xbel_path()
        self._mtime = 0.0

    def poll(self) -> int:
        try:
            stat = self.path.stat()
        except OSError:
            return 0
        if stat.st_mtime == self._mtime or stat.st_size > MAX_BYTES:
            return 0
        self._mtime = stat.st_mtime
        since = self.watermark()
        newest = since
        count = 0
        for ts, path, app in parse_xbel(self.path.read_text(encoding="utf-8", errors="replace")):
            if ts <= since:
                continue
            self.on_file(path, app, ts)
            newest = max(newest, ts)
            count += 1
        if newest > since:
            self.set_watermark(newest)
        return count

    def run(self) -> None:
        while not self.stop.is_set():
            try:
                self.poll()
            except OSError as exc:
                log.warning("files: %s", exc)
            self.stop.wait(self.interval)
