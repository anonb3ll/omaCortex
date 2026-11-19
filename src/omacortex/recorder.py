"""Turns sensor readings into spans in the store.

Collectors report *what is true now* ("this window is focused", "these
tracks are playing"); the recorder works out which spans to open and close.
Every reading goes through the privacy guard and the consent/pause state
first, so a collector never writes to the store directly.
"""

from __future__ import annotations

import threading
import time
from dataclasses import dataclass

from omacortex.privacy import Privacy
from omacortex.state import State
from omacortex.store import Store


@dataclass(frozen=True)
class Window:
    app: str
    title: str
    workspace: str = ""


@dataclass(frozen=True)
class Track:
    player: str
    title: str
    artist: str = ""
    url: str = ""

    @property
    def key(self) -> tuple[str, str, str]:
        return (self.player, self.title, self.artist)


class Recorder:
    def __init__(
        self,
        store: Store,
        privacy: Privacy,
        state: State,
        *,
        min_span_seconds: float = 2,
        clock=time.time,
    ) -> None:
        self.store = store
        self.privacy = privacy
        self.state = state
        self.min_span = min_span_seconds
        self.clock = clock
        self._lock = threading.RLock()
        self._focus_id: int | None = None
        self._focus_key: tuple[str, str] | None = None
        self._window: Window | None = None  # last window reported, even while away
        self._media: dict[tuple[str, str, str], int] = {}
        self._idle = False
        self._recording = state.read().recording

    # -- state ------------------------------------------------------------

    @property
    def recording(self) -> bool:
        return self._recording and not self._idle

    def refresh_state(self, at: float | None = None) -> bool:
        """Re-read consent/pause. Closes everything when recording stops."""
        at = self.clock() if at is None else at
        with self._lock:
            now_recording = self.state.read().recording
            if now_recording == self._recording:
                return now_recording
            self._recording = now_recording
            if not now_recording:
                self._close_focus(at)
                self._close_media(at)
                self.store.add_event("marker", "omacortex", title="paused", at=at)
            else:
                self.store.add_event("marker", "omacortex", title="resumed", at=at)
                if self._window and not self._idle:
                    self._open_focus(self._window, at)
            return now_recording

    def set_idle(self, idle: bool, at: float | None = None) -> None:
        at = self.clock() if at is None else at
        with self._lock:
            if idle == self._idle:
                return
            self._idle = idle
            if idle:
                # Focus ends when you walk away; media keeps playing (you may be watching).
                self._close_focus(at)
                if self._recording:
                    self.store.add_event("marker", "omacortex", title="away", at=at)
            elif self._recording:
                self.store.add_event("marker", "omacortex", title="back", at=at)
                if self._window:
                    self._open_focus(self._window, at)

    # -- readings ---------------------------------------------------------

    def focus(self, window: Window | None, at: float | None = None) -> None:
        at = self.clock() if at is None else at
        with self._lock:
            self._window = window
            if not self.recording:
                return
            if window is None or not (window.app or window.title):
                self._close_focus(at)
                return
            if self.privacy.window_excluded(window.app, window.title, window.workspace):
                self._close_focus(at)
                return
            self._open_focus(window, at)

    def media(self, tracks: list[Track], at: float | None = None) -> None:
        at = self.clock() if at is None else at
        with self._lock:
            if not self._recording:
                self._close_media(at)
                return
            wanted = {}
            for track in tracks:
                if not track.title or self.privacy.title_excluded(track.title):
                    continue
                wanted[track.key] = track
            for key in list(self._media):
                if key not in wanted:
                    self.store.close_span(self._media.pop(key), at, min_seconds=self.min_span)
            for key, track in wanted.items():
                if key in self._media:
                    continue
                self._media[key] = self.store.open_span(
                    "media",
                    "mpris",
                    app=self.privacy.clean(track.player),
                    title=self.privacy.clean(track.title),
                    detail=self.privacy.clean(" · ".join(p for p in (track.artist, track.url) if p)),
                    at=at,
                )

    def file_opened(self, path: str, app: str = "", at: float | None = None) -> bool:
        at = self.clock() if at is None else at
        with self._lock:
            if not self._recording or self.privacy.path_excluded(path):
                return False
            name = path.rsplit("/", 1)[-1]
            self.store.add_event(
                "file",
                "recent-files",
                app=self.privacy.clean(app),
                title=self.privacy.clean(name),
                detail=self.privacy.clean(path),
                at=at,
            )
            return True

    def note(self, text: str, source: str = "cli", at: float | None = None) -> int | None:
        """A note is something you chose to record, so it is kept even while paused."""
        at = self.clock() if at is None else at
        text = self.privacy.clean(text)
        if not text:
            return None
        with self._lock:
            return self.store.add_event("note", source, title=text, at=at)

    def shutdown(self, at: float | None = None) -> None:
        at = self.clock() if at is None else at
        with self._lock:
            self._close_focus(at)
            self._close_media(at)

    # -- internals --------------------------------------------------------

    def _open_focus(self, window: Window, at: float) -> None:
        app = self.privacy.clean(window.app)
        title = self.privacy.clean(window.title)
        key = (app, title)
        if key == self._focus_key and self._focus_id is not None:
            return
        self._close_focus(at)
        self._focus_id = self.store.open_span(
            "focus", "hyprland", app=app, title=title, detail=window.workspace, at=at
        )
        self._focus_key = key

    def _close_focus(self, at: float) -> None:
        if self._focus_id is not None:
            self.store.close_span(self._focus_id, at, min_seconds=self.min_span)
        self._focus_id = None
        self._focus_key = None

    def _close_media(self, at: float) -> None:
        for span_id in self._media.values():
            self.store.close_span(span_id, at, min_seconds=self.min_span)
        self._media.clear()
