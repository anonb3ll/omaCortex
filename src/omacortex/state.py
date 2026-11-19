"""Consent and pause state, kept as small files in the state directory.

Recording happens only when consent has been given *and* capture is not
paused. Both are plain files so the CLI, the shell plugin and the daemon agree
without talking to each other.
"""

from __future__ import annotations

import json
import os
import time
from dataclasses import dataclass
from pathlib import Path

from omacortex.config import Paths


def _read(path: Path) -> dict:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
        return data if isinstance(data, dict) else {}
    except (OSError, ValueError):
        return {}


def _write(path: Path, data: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    fd = os.open(tmp, os.O_CREAT | os.O_WRONLY | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        json.dump(data, handle)
    os.replace(tmp, path)


@dataclass(frozen=True)
class RecordingState:
    consent: bool
    paused: bool
    paused_until: float | None

    @property
    def recording(self) -> bool:
        return self.consent and not self.paused

    def as_dict(self) -> dict:
        return {
            "consent": self.consent,
            "paused": self.paused,
            "paused_until": self.paused_until,
            "recording": self.recording,
        }


class State:
    def __init__(self, paths: Paths, *, clock=time.time) -> None:
        self.paths = paths
        self.clock = clock

    def read(self) -> RecordingState:
        consent = _read(self.paths.consent_file).get("granted") is True
        pause = _read(self.paths.pause_file)
        paused = pause.get("paused") is True
        until = pause.get("until")
        until = float(until) if isinstance(until, (int, float)) else None
        if paused and until is not None and until <= self.clock():
            paused, until = False, None
        return RecordingState(consent=consent, paused=paused, paused_until=until)

    def grant(self) -> None:
        _write(self.paths.consent_file, {"granted": True, "at": self.clock()})

    def revoke(self) -> None:
        _write(self.paths.consent_file, {"granted": False, "at": self.clock()})

    def pause(self, seconds: float | None = None) -> None:
        until = self.clock() + seconds if seconds else None
        _write(self.paths.pause_file, {"paused": True, "until": until, "at": self.clock()})

    def resume(self) -> None:
        _write(self.paths.pause_file, {"paused": False, "at": self.clock()})
