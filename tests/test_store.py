import os
import stat

import pytest

from omacortex.store import Store, fts_query


def test_database_file_is_private(tmp_path):
    db = tmp_path / "x" / "cortex.db"
    Store(db)
    assert stat.S_IMODE(os.stat(db).st_mode) == 0o600


def test_spans_open_close_and_short_ones_are_dropped(clock):
    s = Store(":memory:", clock=clock)
    a = s.open_span("focus", "hyprland", app="kitty", title="nvim")
    clock.tick(1)
    assert s.close_span(a, min_seconds=2) is False
    b = s.open_span("focus", "hyprland", app="kitty", title="nvim")
    clock.tick(10)
    assert s.close_span(b, min_seconds=2) is True
    rows = s.timeline(start=0)
    assert len(rows) == 1 and rows[0]["seconds"] == 10 and not rows[0]["ongoing"]


def test_open_span_reads_as_ongoing_until_now(clock):
    s = Store(":memory:", clock=clock)
    s.open_span("focus", "hyprland", app="chromium", title="Docs")
    clock.tick(42)
    cur = s.current()
    assert cur["ongoing"] and cur["seconds"] == 42


def test_search_matches_prefixes_and_survives_hostile_queries(clock):
    s = Store(":memory:", clock=clock)
    s.add_event("note", "cli", title="Finished the quarterly report draft")
    s.add_event("file", "recent-files", title="budget.ods", detail="/home/u/budget.ods")
    assert [r["title"] for r in s.search("quart rep")] == ["Finished the quarterly report draft"]
    assert s.search("budget")[0]["kind"] == "file"
    for nasty in [
        '"',
        "NEAR(",
        "a OR",
        "*",
        ")",
        "title:",
        "'; DROP TABLE activity; --",
    ]:
        s.search(nasty)  # must not raise
    assert fts_query("  ") == ""


def test_usage_clips_spans_to_the_window(clock):
    s = Store(":memory:", clock=clock)
    t0 = clock()
    a = s.open_span("focus", "h", app="kitty", at=t0)
    s.close_span(a, at=t0 + 100)
    b = s.open_span("focus", "h", app="chromium", at=t0 + 100)
    s.close_span(b, at=t0 + 400)
    usage = {u["app"]: u["seconds"] for u in s.usage(start=t0 + 50, end=t0 + 200)}
    assert usage == {"kitty": 50, "chromium": 100}


def test_forget_and_retention(clock):
    s = Store(":memory:", clock=clock)
    t0 = clock()
    old = s.open_span("focus", "h", app="kitty", at=t0 - 40 * 86400)
    s.close_span(old, at=t0 - 40 * 86400 + 60)
    s.add_event("note", "cli", title="recent", at=t0 - 60)
    s.add_event("note", "cli", title="secret plan", at=t0 - 5)
    s.put_summary("2026-01-01", "old summary", "m")
    assert s.purge_older_than(t0 - 30 * 86400) == 1
    assert s.forget(start=t0 - 30) == 1
    assert [r["title"] for r in s.timeline(start=0)] == ["recent"]
    assert s.search("secret") == []
    with pytest.raises(ValueError):
        s.forget()
    assert s.forget(everything=True) == 1
    assert s.timeline(start=0) == []


def test_forget_by_app_is_case_insensitive(clock):
    s = Store(":memory:", clock=clock)
    s.add_event("focus", "h", app="Signal", title="chat")
    s.add_event("focus", "h", app="kitty", title="shell")
    assert s.forget(app="signal") == 1
    assert [r["app"] for r in s.timeline(start=0)] == ["kitty"]
