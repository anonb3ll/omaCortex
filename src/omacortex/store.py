"""The activity store: one SQLite file with full-text search.

Every row in ``activity`` is either a span (focus on a window, a track
playing) with ``started_at``/``ended_at``, or a point event (a file opened, a
note) whose ``ended_at`` equals ``started_at``. A span still in progress has
``ended_at`` NULL; readers treat it as ending now.
"""

from __future__ import annotations

import json
import os
import re
import sqlite3
import threading
import time
from collections.abc import Iterable, Sequence
from pathlib import Path
from typing import Any

SCHEMA_VERSION = 1
KINDS = ("focus", "media", "file", "note", "marker")

_SCHEMA = """
CREATE TABLE IF NOT EXISTS activity (
  id INTEGER PRIMARY KEY,
  kind TEXT NOT NULL,
  source TEXT NOT NULL,
  app TEXT NOT NULL DEFAULT '',
  title TEXT NOT NULL DEFAULT '',
  detail TEXT NOT NULL DEFAULT '',
  started_at REAL NOT NULL,
  ended_at REAL,
  meta TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS activity_started ON activity(started_at);
CREATE INDEX IF NOT EXISTS activity_kind_started ON activity(kind, started_at);
CREATE INDEX IF NOT EXISTS activity_open ON activity(ended_at) WHERE ended_at IS NULL;

CREATE VIRTUAL TABLE IF NOT EXISTS activity_fts USING fts5(
  app, title, detail, content='activity', content_rowid='id', tokenize='unicode61'
);
CREATE TRIGGER IF NOT EXISTS activity_ai AFTER INSERT ON activity BEGIN
  INSERT INTO activity_fts(rowid, app, title, detail) VALUES (new.id, new.app, new.title, new.detail);
END;
CREATE TRIGGER IF NOT EXISTS activity_ad AFTER DELETE ON activity BEGIN
  INSERT INTO activity_fts(activity_fts, rowid, app, title, detail)
  VALUES ('delete', old.id, old.app, old.title, old.detail);
END;
CREATE TRIGGER IF NOT EXISTS activity_au AFTER UPDATE OF app, title, detail ON activity BEGIN
  INSERT INTO activity_fts(activity_fts, rowid, app, title, detail)
  VALUES ('delete', old.id, old.app, old.title, old.detail);
  INSERT INTO activity_fts(rowid, app, title, detail) VALUES (new.id, new.app, new.title, new.detail);
END;

CREATE TABLE IF NOT EXISTS summaries (
  day TEXT NOT NULL,
  scope TEXT NOT NULL DEFAULT 'day',
  model TEXT NOT NULL DEFAULT '',
  text TEXT NOT NULL,
  created_at REAL NOT NULL,
  PRIMARY KEY (day, scope)
);

CREATE TABLE IF NOT EXISTS meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
"""

_COLUMNS = "id, kind, source, app, title, detail, started_at, ended_at, meta"


def _row(row: Sequence[Any], now: float) -> dict[str, Any]:
    (rid, kind, source, app, title, detail, started, ended, meta) = row
    end = ended if ended is not None else now
    try:
        meta_obj = json.loads(meta) if meta else {}
    except ValueError:
        meta_obj = {}
    return {
        "id": rid,
        "kind": kind,
        "source": source,
        "app": app,
        "title": title,
        "detail": detail,
        "started_at": started,
        "ended_at": end,
        "ongoing": ended is None,
        "seconds": max(0.0, round(end - started, 1)),
        "meta": meta_obj,
    }


def fts_query(text: str) -> str:
    """Turn free text into a safe FTS5 query: every word must match, as a prefix."""
    words = re.findall(r"[\w.-]+", text or "", flags=re.UNICODE)[:12]
    return " ".join('"' + w.replace('"', "") + '"*' for w in words if w.strip(".-"))


class Store:
    def __init__(self, path: Path | str, *, clock=time.time) -> None:
        self.path = str(path)
        self.clock = clock
        self._lock = threading.RLock()
        if self.path != ":memory:":
            Path(self.path).parent.mkdir(parents=True, exist_ok=True)
            # Create the file 0600 before SQLite opens it.
            fd = os.open(self.path, os.O_CREAT | os.O_RDWR, 0o600)
            os.close(fd)
            os.chmod(self.path, 0o600)
        self.conn = sqlite3.connect(self.path, timeout=10, check_same_thread=False)
        self.conn.execute("PRAGMA journal_mode=WAL")
        self.conn.execute("PRAGMA synchronous=NORMAL")
        self.conn.execute("PRAGMA foreign_keys=ON")
        self.conn.execute("PRAGMA secure_delete=ON")
        with self.conn:
            self.conn.executescript(_SCHEMA)
            self.conn.execute(f"PRAGMA user_version={SCHEMA_VERSION}")

    def close(self) -> None:
        with self._lock:
            self.conn.close()

    # -- writes ---------------------------------------------------------

    def open_span(
        self,
        kind: str,
        source: str,
        *,
        app: str = "",
        title: str = "",
        detail: str = "",
        at: float | None = None,
        meta: dict[str, Any] | None = None,
    ) -> int:
        at = self.clock() if at is None else at
        with self._lock, self.conn:
            cur = self.conn.execute(
                "INSERT INTO activity(kind, source, app, title, detail, started_at, ended_at, meta)"
                " VALUES (?, ?, ?, ?, ?, ?, NULL, ?)",
                (kind, source, app, title, detail, at, json.dumps(meta or {})),
            )
            return int(cur.lastrowid)

    def close_span(self, span_id: int, at: float | None = None, *, min_seconds: float = 0) -> bool:
        """End a span. Spans shorter than ``min_seconds`` are deleted. Returns True if kept."""
        at = self.clock() if at is None else at
        with self._lock, self.conn:
            row = self.conn.execute(
                "SELECT started_at FROM activity WHERE id = ? AND ended_at IS NULL",
                (span_id,),
            ).fetchone()
            if row is None:
                return False
            if at - row[0] < min_seconds:
                self.conn.execute("DELETE FROM activity WHERE id = ?", (span_id,))
                return False
            self.conn.execute(
                "UPDATE activity SET ended_at = ? WHERE id = ?",
                (max(at, row[0]), span_id),
            )
            return True

    def add_event(
        self,
        kind: str,
        source: str,
        *,
        app: str = "",
        title: str = "",
        detail: str = "",
        at: float | None = None,
        meta: dict[str, Any] | None = None,
    ) -> int:
        at = self.clock() if at is None else at
        with self._lock, self.conn:
            cur = self.conn.execute(
                "INSERT INTO activity(kind, source, app, title, detail, started_at, ended_at, meta)"
                " VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                (kind, source, app, title, detail, at, at, json.dumps(meta or {})),
            )
            return int(cur.lastrowid)

    def close_all_open(self, at: float | None = None) -> int:
        at = self.clock() if at is None else at
        with self._lock, self.conn:
            cur = self.conn.execute(
                "UPDATE activity SET ended_at = MAX(started_at, ?) WHERE ended_at IS NULL",
                (at,),
            )
            return cur.rowcount

    def set_meta(self, key: str, value: str) -> None:
        with self._lock, self.conn:
            self.conn.execute(
                "INSERT INTO meta(key, value) VALUES (?, ?)"
                " ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                (key, value),
            )

    def get_meta(self, key: str, default: str = "") -> str:
        with self._lock:
            row = self.conn.execute("SELECT value FROM meta WHERE key = ?", (key,)).fetchone()
        return row[0] if row else default

    def forget(
        self,
        *,
        start: float | None = None,
        end: float | None = None,
        app: str | None = None,
        everything: bool = False,
    ) -> int:
        """Delete activity. A span is removed when any part of it overlaps the window."""
        if not everything and start is None and end is None and not app:
            raise ValueError("forget needs a time window, an app, or everything=True")
        clauses: list[str] = []
        params: list[Any] = []
        if not everything:
            if start is not None:
                clauses.append("COALESCE(ended_at, ?) >= ?")
                params += [self.clock(), start]
            if end is not None:
                clauses.append("started_at <= ?")
                params.append(end)
            if app:
                clauses.append("lower(app) = lower(?)")
                params.append(app)
        where = " AND ".join(clauses) if clauses else "1=1"
        with self._lock, self.conn:
            cur = self.conn.execute(f"DELETE FROM activity WHERE {where}", params)
            if everything:
                self.conn.execute("DELETE FROM summaries")
            elif start is not None or end is not None:
                # Cached summaries may quote forgotten activity.
                self.conn.execute("DELETE FROM summaries")
            count = cur.rowcount
        if everything:
            with self._lock:
                with self.conn:
                    self.conn.execute("INSERT INTO activity_fts(activity_fts) VALUES ('rebuild')")
                # Reclaim the freed pages so deleted text is gone from the file too.
                self.conn.execute("VACUUM")
        return count

    def purge_older_than(self, cutoff: float) -> int:
        with self._lock, self.conn:
            cur = self.conn.execute(
                "DELETE FROM activity WHERE ended_at IS NOT NULL AND ended_at < ?",
                (cutoff,),
            )
            day = time.strftime("%Y-%m-%d", time.localtime(cutoff))
            self.conn.execute("DELETE FROM summaries WHERE day < ?", (day,))
            return cur.rowcount

    # -- reads ----------------------------------------------------------

    def timeline(
        self,
        *,
        start: float | None = None,
        end: float | None = None,
        kinds: Iterable[str] | None = None,
        min_seconds: float = 0,
        limit: int = 200,
        newest_first: bool = False,
    ) -> list[dict[str, Any]]:
        now = self.clock()
        clauses = ["1=1"]
        params: list[Any] = []
        if start is not None:
            clauses.append("COALESCE(ended_at, ?) >= ?")
            params += [now, start]
        if end is not None:
            clauses.append("started_at <= ?")
            params.append(end)
        kinds = [k for k in (kinds or []) if k in KINDS]
        if kinds:
            clauses.append(f"kind IN ({','.join('?' * len(kinds))})")
            params += kinds
        if min_seconds > 0:
            clauses.append("(COALESCE(ended_at, ?) - started_at >= ? OR kind IN ('file','note','marker'))")
            params += [now, min_seconds]
        order = "DESC" if newest_first else "ASC"
        sql = (
            f"SELECT {_COLUMNS} FROM activity WHERE {' AND '.join(clauses)}"
            f" ORDER BY started_at {order}, id {order} LIMIT ?"
        )
        params.append(max(1, int(limit)))
        with self._lock:
            rows = self.conn.execute(sql, params).fetchall()
        return [_row(r, now) for r in rows]

    def current(self, kind: str = "focus") -> dict[str, Any] | None:
        now = self.clock()
        with self._lock:
            row = self.conn.execute(
                f"SELECT {_COLUMNS} FROM activity WHERE kind = ? AND ended_at IS NULL"
                " ORDER BY started_at DESC LIMIT 1",
                (kind,),
            ).fetchone()
        return _row(row, now) if row else None

    def search(
        self,
        query: str,
        *,
        start: float | None = None,
        end: float | None = None,
        kinds: Iterable[str] | None = None,
        limit: int = 50,
    ) -> list[dict[str, Any]]:
        match = fts_query(query)
        if not match:
            return []
        now = self.clock()
        clauses = ["activity_fts MATCH ?"]
        params: list[Any] = [match]
        if start is not None:
            clauses.append("COALESCE(a.ended_at, ?) >= ?")
            params += [now, start]
        if end is not None:
            clauses.append("a.started_at <= ?")
            params.append(end)
        kinds = [k for k in (kinds or []) if k in KINDS]
        if kinds:
            clauses.append(f"a.kind IN ({','.join('?' * len(kinds))})")
            params += kinds
        cols = ", ".join("a." + c.strip() for c in _COLUMNS.split(","))
        sql = (
            f"SELECT {cols} FROM activity_fts JOIN activity a ON a.id = activity_fts.rowid"
            f" WHERE {' AND '.join(clauses)} ORDER BY a.started_at DESC LIMIT ?"
        )
        params.append(max(1, int(limit)))
        with self._lock:
            rows = self.conn.execute(sql, params).fetchall()
        return [_row(r, now) for r in rows]

    def usage(self, *, start: float, end: float | None = None, limit: int = 25) -> list[dict[str, Any]]:
        """Seconds of focus per app inside [start, end], clipped to the window."""
        now = self.clock()
        end = now if end is None else end
        with self._lock:
            rows = self.conn.execute(
                "SELECT app, SUM(MIN(COALESCE(ended_at, ?), ?) - MAX(started_at, ?)) AS secs,"
                " COUNT(*) FROM activity WHERE kind = 'focus' AND COALESCE(ended_at, ?) >= ?"
                " AND started_at <= ? GROUP BY app ORDER BY secs DESC LIMIT ?",
                (now, end, start, now, start, end, max(1, int(limit))),
            ).fetchall()
        return [{"app": a, "seconds": round(max(0.0, s or 0.0), 1), "spans": n} for a, s, n in rows]

    def stats(self) -> dict[str, Any]:
        with self._lock:
            counts = dict(self.conn.execute("SELECT kind, COUNT(*) FROM activity GROUP BY kind").fetchall())
            first = self.conn.execute("SELECT MIN(started_at) FROM activity").fetchone()[0]
        size = 0
        if self.path != ":memory:":
            for suffix in ("", "-wal", "-shm"):
                try:
                    size += os.path.getsize(self.path + suffix)
                except OSError:
                    pass
        return {"rows": counts, "oldest": first, "bytes": size}

    def get_summary(self, day: str, scope: str = "day") -> dict[str, Any] | None:
        with self._lock:
            row = self.conn.execute(
                "SELECT day, scope, model, text, created_at FROM summaries WHERE day = ? AND scope = ?",
                (day, scope),
            ).fetchone()
        if not row:
            return None
        return dict(zip(("day", "scope", "model", "text", "created_at"), row, strict=True))

    def put_summary(self, day: str, text: str, model: str, scope: str = "day") -> None:
        with self._lock, self.conn:
            self.conn.execute(
                "INSERT INTO summaries(day, scope, model, text, created_at) VALUES (?, ?, ?, ?, ?)"
                " ON CONFLICT(day, scope) DO UPDATE SET model = excluded.model,"
                " text = excluded.text, created_at = excluded.created_at",
                (day, scope, model, text, self.clock()),
            )

    def export(self) -> Iterable[dict[str, Any]]:
        now = self.clock()
        with self._lock:
            rows = self.conn.execute(f"SELECT {_COLUMNS} FROM activity ORDER BY started_at").fetchall()
        for r in rows:
            yield _row(r, now)
