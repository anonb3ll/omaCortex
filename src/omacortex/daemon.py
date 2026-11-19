"""The capture daemon.

One process per user (guarded by a lock file). It starts the enabled
sensors in threads, re-reads consent and pause state every couple of seconds,
writes a heartbeat so a crash never leaves a span open forever, enforces the
retention window, and listens on a private Unix socket for "away"/"back"
reports from the shell plugin.
"""

from __future__ import annotations

import fcntl
import json
import logging
import os
import signal
import socket
import threading
import time

from omacortex.collectors.files import FilesCollector
from omacortex.collectors.hyprland import HyprlandCollector
from omacortex.collectors.media import MediaCollector
from omacortex.config import Config
from omacortex.privacy import Privacy
from omacortex.recorder import Recorder
from omacortex.state import State
from omacortex.store import Store

log = logging.getLogger("omacortex")

TICK = 2.0
HEARTBEAT_EVERY = 30.0
PURGE_EVERY = 3600.0


class AlreadyRunning(RuntimeError):
    pass


def acquire_lock(path) -> object:
    handle = open(path, "a+")  # noqa: SIM115 - held for the life of the process
    try:
        fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError as exc:
        handle.close()
        raise AlreadyRunning(str(path)) from exc
    os.chmod(path, 0o600)
    return handle


class Daemon:
    def __init__(self, cfg: Config, *, clock=time.time) -> None:
        self.cfg = cfg
        self.clock = clock
        self.stop = threading.Event()
        cfg.paths.ensure()
        self.store = Store(cfg.paths.db, clock=clock)
        self.state = State(cfg.paths, clock=clock)
        self.recorder = Recorder(
            self.store,
            Privacy.from_config(cfg),
            self.state,
            min_span_seconds=float(cfg.get("capture", "min_span_seconds")),
            clock=clock,
        )
        self.threads: list[threading.Thread] = []
        self._server: socket.socket | None = None

    # -- lifecycle ----------------------------------------------------------

    def recover(self) -> None:
        """Close spans left open by a crash at the last heartbeat."""
        beat = self.store.get_meta("heartbeat")
        at = float(beat) if beat else self.clock()
        closed = self.store.close_all_open(min(at, self.clock()))
        if closed:
            log.info("closed %d span(s) left open by a previous run", closed)
        self.store.set_meta("away", "0")

    def start_sensors(self) -> None:
        def cap(key: str) -> bool:
            return bool(self.cfg.get("capture", key))

        if cap("windows"):
            self._spawn("hyprland", HyprlandCollector(self.recorder.focus, self.stop).run)
        if cap("media"):
            interval = float(self.cfg.get("capture", "media_poll_seconds"))
            self._spawn("media", MediaCollector(self.recorder.media, self.stop, interval).run)
        if cap("files"):
            interval = float(self.cfg.get("capture", "files_poll_seconds"))
            mark = self.store.get_meta("files_watermark")
            if not mark:
                # Do not import the whole history of the recent list on first run.
                self.store.set_meta("files_watermark", str(self.clock()))
            collector = FilesCollector(
                on_file=lambda path, app, ts: self.recorder.file_opened(path, app, ts),
                watermark=lambda: float(self.store.get_meta("files_watermark") or self.clock()),
                set_watermark=lambda ts: self.store.set_meta("files_watermark", str(ts)),
                stop=self.stop,
                interval=interval,
            )
            self._spawn("files", collector.run)
        self._spawn("control", self.serve_control)

    def _spawn(self, name: str, target) -> None:
        thread = threading.Thread(target=self._guard(name, target), name=name, daemon=True)
        thread.start()
        self.threads.append(thread)

    def _guard(self, name, target):
        def run():
            try:
                target()
            except Exception:  # a sensor bug must not take the daemon down
                log.exception("%s sensor stopped", name)

        return run

    def run(self) -> int:
        lock = acquire_lock(self.cfg.paths.lock_file)
        try:
            self.recover()
            self.start_sensors()
            last_beat = last_purge = 0.0
            while not self.stop.is_set():
                now = self.clock()
                self.recorder.refresh_state(now)
                if now - last_beat >= HEARTBEAT_EVERY:
                    self.store.set_meta("heartbeat", str(now))
                    last_beat = now
                if now - last_purge >= PURGE_EVERY:
                    self.purge(now)
                    last_purge = now
                self.stop.wait(TICK)
        finally:
            self.shutdown()
            lock.close()
        return 0

    def purge(self, now: float) -> int:
        days = float(self.cfg.get("privacy", "retention_days") or 0)
        if days <= 0:
            return 0
        removed = self.store.purge_older_than(now - days * 86400)
        if removed:
            log.info("retention: removed %d row(s) older than %g days", removed, days)
        return removed

    def shutdown(self) -> None:
        self.stop.set()
        self.recorder.shutdown()
        self.store.set_meta("heartbeat", str(self.clock()))
        if self._server:
            self._server.close()
        try:
            self.cfg.paths.control_socket.unlink()
        except OSError:
            pass

    # -- control socket --------------------------------------------------------

    def handle(self, message: dict) -> dict:
        cmd = message.get("cmd")
        if cmd == "away":
            self.recorder.set_idle(True)
            self.store.set_meta("away", "1")
        elif cmd == "back":
            self.recorder.set_idle(False)
            self.store.set_meta("away", "0")
        elif cmd == "refresh":
            self.recorder.refresh_state()
        elif cmd != "ping":
            return {"ok": False, "error": f"unknown command: {cmd!r}"}
        return {"ok": True, "recording": self.recorder.recording}

    def serve_control(self) -> None:
        path = self.cfg.paths.control_socket
        try:
            path.unlink()
        except OSError:
            pass
        server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        old = os.umask(0o177)
        try:
            server.bind(str(path))
        except OSError as exc:
            log.error("control socket unavailable (%s); away/back reports are ignored", exc)
            server.close()
            return
        finally:
            os.umask(old)
        server.listen(4)
        server.settimeout(1.0)
        self._server = server
        while not self.stop.is_set():
            try:
                conn, _ = server.accept()
            except TimeoutError:
                continue
            except OSError:
                break
            with conn:
                conn.settimeout(2.0)
                try:
                    raw = conn.recv(4096).split(b"\n", 1)[0]
                    reply = self.handle(json.loads(raw or b"{}"))
                except (OSError, ValueError) as exc:
                    reply = {"ok": False, "error": str(exc)}
                try:
                    conn.sendall(json.dumps(reply).encode() + b"\n")
                except OSError:
                    pass


def send_control(cfg: Config, cmd: str, timeout: float = 2.0) -> dict:
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        sock.settimeout(timeout)
        sock.connect(str(cfg.paths.control_socket))
        sock.sendall(json.dumps({"cmd": cmd}).encode() + b"\n")
        data = sock.recv(4096)
    return json.loads(data or b"{}")


def run_daemon(cfg: Config, *, if_not_running: bool = False) -> int:
    logging.basicConfig(level=logging.INFO, format="omacortex: %(message)s")
    daemon = Daemon(cfg)

    def _stop(*_):
        daemon.stop.set()

    signal.signal(signal.SIGTERM, _stop)
    signal.signal(signal.SIGINT, _stop)
    try:
        return daemon.run()
    except AlreadyRunning:
        if if_not_running:
            return 0
        log.error("another omacortex daemon is already running")
        return 1
