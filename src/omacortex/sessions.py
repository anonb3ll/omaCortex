"""Turn raw focus spans into sessions a person would recognise.

A session is a run of focus on one app. Short detours (under
``detour_seconds``) to another app do not end it, and a gap shorter than
``merge_gap`` between two spans of the same app joins them.
"""

from __future__ import annotations

from collections import defaultdict
from typing import Any


def build_sessions(
    spans: list[dict[str, Any]],
    *,
    merge_gap: float = 120,
    detour_seconds: float = 30,
) -> list[dict[str, Any]]:
    focus = sorted((s for s in spans if s.get("kind") == "focus"), key=lambda s: s["started_at"])
    sessions: list[dict[str, Any]] = []
    for span in focus:
        last = sessions[-1] if sessions else None
        if last and span["app"] == last["app"] and span["started_at"] - last["ended_at"] <= merge_gap:
            _extend(last, span)
            continue
        if last and span["seconds"] < detour_seconds and span["started_at"] - last["ended_at"] <= merge_gap:
            # A brief detour: count it as an interruption of the running session.
            last["interruptions"] += 1
            last["ended_at"] = max(last["ended_at"], span["ended_at"])
            last["ongoing"] = span.get("ongoing", False)
            continue
        sessions.append(
            {
                "app": span["app"],
                "started_at": span["started_at"],
                "ended_at": span["ended_at"],
                "active_seconds": 0.0,
                "interruptions": 0,
                "ongoing": span.get("ongoing", False),
                "_titles": defaultdict(float),
            }
        )
        _extend(sessions[-1], span)

    out = []
    for s in sessions:
        titles = sorted(s.pop("_titles").items(), key=lambda kv: kv[1], reverse=True)
        s["titles"] = [{"title": t, "seconds": round(sec, 1)} for t, sec in titles[:5] if t]
        s["title"] = s["titles"][0]["title"] if s["titles"] else ""
        s["active_seconds"] = round(s["active_seconds"], 1)
        out.append(s)
    return out


def _extend(session: dict[str, Any], span: dict[str, Any]) -> None:
    session["ended_at"] = max(session["ended_at"], span["ended_at"])
    session["active_seconds"] += span["seconds"]
    session["_titles"][span.get("title", "")] += span["seconds"]
    session["ongoing"] = span.get("ongoing", False)
