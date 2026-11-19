"""Small, dependency-free time helpers shared by the CLI and the MCP server."""

from __future__ import annotations

import re
import time
from datetime import datetime, timedelta

_DURATION = re.compile(r"^\s*(\d+(?:\.\d+)?)\s*([smhdw]?)\s*$", re.IGNORECASE)
_UNITS = {"": 60, "s": 1, "m": 60, "h": 3600, "d": 86400, "w": 604800}


def parse_duration(text: str) -> float:
    """'90s', '15m', '2h', '1d', '1w'. A bare number means minutes."""
    match = _DURATION.match(str(text))
    if not match:
        raise ValueError(f"not a duration: {text!r} (try 15m, 2h, 1d)")
    return float(match.group(1)) * _UNITS[match.group(2).lower()]


def start_of_day(ts: float) -> float:
    d = datetime.fromtimestamp(ts)
    return d.replace(hour=0, minute=0, second=0, microsecond=0).timestamp()


def parse_when(text: str | float | None, *, now: float | None = None) -> float | None:
    """Parse a point in time.

    Accepts epoch seconds, 'now', 'today', 'yesterday', a duration meaning
    "that long ago" ('2h'), an ISO date ('2026-11-19') or datetime.
    """
    if text is None or text == "":
        return None
    now = time.time() if now is None else now
    if isinstance(text, (int, float)):
        return float(text)
    value = str(text).strip().lower()
    if value == "now":
        return now
    if value == "today":
        return start_of_day(now)
    if value == "yesterday":
        return start_of_day(now) - 86400
    try:
        return now - parse_duration(value)
    except ValueError:
        pass
    try:
        return (
            float(value)
            if re.fullmatch(r"\d{9,}(\.\d+)?", value)
            else datetime.fromisoformat(str(text).strip()).timestamp()
        )
    except ValueError as exc:
        raise ValueError(f"not a time: {text!r} (try 'today', '2h', or 2026-11-19T09:00)") from exc


def day_bounds(day: str | None, *, now: float | None = None) -> tuple[str, float, float]:
    """Return (YYYY-MM-DD, start, end) for 'today', 'yesterday' or an ISO date."""
    now = time.time() if now is None else now
    if not day or day == "today":
        start = start_of_day(now)
    elif day == "yesterday":
        start = start_of_day(now) - 86400
    else:
        start = datetime.fromisoformat(day).replace(hour=0, minute=0, second=0).timestamp()
    end_dt = datetime.fromtimestamp(start) + timedelta(days=1)
    return datetime.fromtimestamp(start).strftime("%Y-%m-%d"), start, end_dt.timestamp()


def human_duration(seconds: float) -> str:
    seconds = int(max(0, seconds))
    if seconds < 60:
        return f"{seconds}s"
    minutes = seconds // 60
    if minutes < 60:
        return f"{minutes}m"
    hours, minutes = divmod(minutes, 60)
    return f"{hours}h{minutes:02d}m" if minutes else f"{hours}h"


def clock(ts: float) -> str:
    return datetime.fromtimestamp(ts).strftime("%H:%M")


def iso(ts: float | None) -> str | None:
    if ts is None:
        return None
    return datetime.fromtimestamp(ts).astimezone().isoformat(timespec="seconds")
