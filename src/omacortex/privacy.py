"""What is never recorded, and how recorded text is scrubbed.

The privacy guard runs before anything touches the database. An excluded
window, workspace or path is dropped entirely (the timeline shows a gap),
and every stored string passes through :meth:`Privacy.clean`.
"""

from __future__ import annotations

import fnmatch
import os
import re
from collections.abc import Iterable
from pathlib import Path
from urllib.parse import urlsplit, urlunsplit

MAX_TEXT = 300

_EMAIL = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")
# Well-known credential shapes first, then any long opaque run of characters.
_TOKENS = re.compile(
    r"\b(?:sk-[A-Za-z0-9_-]{16,}|gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}"
    r"|xox[abposr]-[A-Za-z0-9-]{10,}|AKIA[0-9A-Z]{16}|AIza[0-9A-Za-z_-]{30,}"
    r"|eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{5,})"
    r"|\b[A-Za-z0-9_\-]{40,}\b"
)
_URL = re.compile(r"\b[a-z][a-z0-9+.-]*://[^\s]+", re.IGNORECASE)
_CONTROL = re.compile(r"[\x00-\x1f\x7f]")


def _strip_query(match: re.Match[str]) -> str:
    raw = match.group(0)
    try:
        parts = urlsplit(raw)
    except ValueError:
        return "[url]"
    # Keep scheme, host and path; drop credentials, query and fragment.
    host = parts.hostname or ""
    if parts.port:
        host = f"{host}:{parts.port}"
    return urlunsplit((parts.scheme, host, parts.path, "", ""))


def bounded(text: str, limit: int = MAX_TEXT) -> str:
    text = _CONTROL.sub(" ", text or "").strip()
    return text if len(text) <= limit else text[: limit - 1] + "…"


class Privacy:
    def __init__(
        self,
        *,
        redact: bool = True,
        exclude_apps: Iterable[str] = (),
        exclude_titles: Iterable[str] = (),
        exclude_workspaces: Iterable[str] = (),
        exclude_paths: Iterable[str] = (),
    ) -> None:
        self.redact = redact
        self.exclude_apps = [a.lower() for a in exclude_apps if a]
        self.exclude_titles = []
        self.bad_patterns: list[str] = []
        for pattern in exclude_titles:
            try:
                self.exclude_titles.append(re.compile(pattern))
            except re.error:
                # An invalid pattern must not silently allow everything it
                # was meant to block; it is reported by `omacortex doctor`.
                self.bad_patterns.append(pattern)
        self.exclude_workspaces = {w for w in exclude_workspaces if w}
        self.exclude_paths = [os.path.realpath(os.path.expanduser(p)) for p in exclude_paths if p]

    @classmethod
    def from_config(cls, cfg) -> Privacy:
        return cls(
            redact=bool(cfg.get("privacy", "redact")),
            exclude_apps=cfg.get("privacy", "exclude_apps"),
            exclude_titles=cfg.get("privacy", "exclude_titles"),
            exclude_workspaces=cfg.get("privacy", "exclude_workspaces"),
            exclude_paths=cfg.get("privacy", "exclude_paths"),
        )

    def app_excluded(self, app: str) -> bool:
        name = (app or "").lower()
        return any(fnmatch.fnmatchcase(name, pattern) for pattern in self.exclude_apps)

    def title_excluded(self, title: str) -> bool:
        return any(p.search(title or "") for p in self.exclude_titles)

    def window_excluded(self, app: str, title: str, workspace: str = "") -> bool:
        if workspace and workspace in self.exclude_workspaces:
            return True
        return self.app_excluded(app) or self.title_excluded(title)

    def path_excluded(self, path: str) -> bool:
        real = os.path.realpath(os.path.expanduser(path))
        for root in self.exclude_paths:
            if real == root or real.startswith(root.rstrip(os.sep) + os.sep):
                return True
        name = Path(real).name.lower()
        return name.endswith((".key", ".pem", ".kdbx", ".gpg", ".asc")) or name in {
            ".env",
            "id_rsa",
            "id_ed25519",
            ".netrc",
        }

    def clean(self, text: str) -> str:
        text = bounded(text, 2000)
        if self.redact and text:
            text = _URL.sub(_strip_query, text)
            text = _EMAIL.sub("[email]", text)
            text = _TOKENS.sub("[redacted]", text)
        return bounded(text)
