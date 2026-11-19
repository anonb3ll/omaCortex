import pytest

from omacortex.sessions import build_sessions
from omacortex.timeutil import day_bounds, human_duration, parse_duration, parse_when


def span(app, title, start, seconds, ongoing=False):
    return {
        "kind": "focus",
        "app": app,
        "title": title,
        "started_at": start,
        "ended_at": start + seconds,
        "seconds": seconds,
        "ongoing": ongoing,
    }


def test_sessions_merge_same_app_and_absorb_short_detours():
    spans = [
        span("kitty", "nvim a.py", 0, 600),
        span("chromium", "Docs", 600, 10),  # detour
        span("kitty", "nvim b.py", 610, 300),
        span("chromium", "Review PR", 1000, 900),
        span("chromium", "Docs", 1950, 100, ongoing=True),
    ]
    sessions = build_sessions(spans)
    assert [(s["app"], s["active_seconds"], s["interruptions"]) for s in sessions] == [
        ("kitty", 900, 1),
        ("chromium", 1000, 0),
    ]
    assert sessions[0]["title"] == "nvim a.py"
    assert sessions[1]["ongoing"] is True


def test_long_gap_splits_sessions():
    sessions = build_sessions([span("kitty", "a", 0, 60), span("kitty", "a", 1000, 60)])
    assert len(sessions) == 2


def test_parse_time():
    now = 1_800_000_000.0
    assert parse_when("2h", now=now) == now - 7200
    assert parse_when("15m", now=now) == now - 900
    assert parse_when("now", now=now) == now
    assert parse_when(None) is None
    assert parse_when("today", now=now) <= now
    assert parse_when("2026-11-19") > 0
    with pytest.raises(ValueError):
        parse_when("next tuesday")
    assert parse_duration("90s") == 90 and parse_duration("10") == 600
    assert human_duration(59) == "59s" and human_duration(3660) == "1h01m"
    label, start, end = day_bounds("2026-11-19")
    assert label == "2026-11-19" and 82800 <= end - start <= 90000


def test_example_config_matches_defaults():
    import tomllib

    from omacortex.config import DEFAULTS, EXAMPLE_CONFIG

    example = tomllib.loads(EXAMPLE_CONFIG)
    for section, values in example.items():
        for key, value in values.items():
            assert DEFAULTS[section][key] == value, (section, key)
