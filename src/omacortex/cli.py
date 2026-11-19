"""The ``omacortex`` command."""

from __future__ import annotations

import argparse
import json
import shutil
import sys
import time

from omacortex import __version__
from omacortex.config import EXAMPLE_CONFIG
from omacortex.config import load as load_config
from omacortex.timeutil import clock, human_duration, iso, parse_duration, parse_when


def _cortex():
    from omacortex.api import Cortex

    return Cortex()


def _print_json(data) -> None:
    print(json.dumps(data, indent=2, ensure_ascii=False, default=str))


def _poke(cfg, cmd: str) -> None:
    """Tell a running daemon to re-read state now; fine if none is running."""
    from omacortex.daemon import send_control

    try:
        send_control(cfg, cmd, timeout=1.0)
    except (OSError, ValueError):
        pass


def _row_line(r: dict) -> str:
    span = "" if r["kind"] in ("file", "note", "marker") else f" {human_duration(r['seconds']):>6}"
    who = f"{r['app']}: " if r["app"] else ""
    flag = " (now)" if r.get("ongoing") else ""
    return f"{time.strftime('%m-%d %H:%M', time.localtime(r['started_at']))}{span}  [{r['kind']}] {who}{r['title']}{flag}"


# -- commands ------------------------------------------------------------------


def cmd_daemon(args) -> int:
    from omacortex.daemon import run_daemon

    return run_daemon(load_config(), if_not_running=args.if_not_running)


def cmd_status(args) -> int:
    c = _cortex()
    s = c.status()
    if args.json:
        _print_json(s)
        return 0
    if not s["consent"]:
        state = "off: not enabled yet (run `omacortex enable`)"
    elif s["paused"]:
        state = "paused" + (f" until {clock(s['paused_until'])}" if s["paused_until"] else "")
    elif s["away"]:
        state = "recording (you are away)"
    else:
        state = "recording"
    print(f"omaCortex {s['version']}: {state}")
    print(f"daemon: {'running' if s['daemon'] else 'not running'}")
    on = [k for k, v in s["sources"].items() if v]
    print(f"sensors: {', '.join(on) or 'none'}")
    print(f"stored: {sum(s['rows'].values())} rows ({s['db_bytes'] // 1024} KiB), since {s['oldest'] or '-'}")
    print(f"retention: {s['retention_days'] or 'forever'} days")
    if s["config_error"]:
        print(f"config error (defaults in use): {s['config_error']}")
    return 0


def cmd_enable(args) -> int:
    c = _cortex()
    c.state.grant()
    c.state.resume()
    _poke(c.cfg, "refresh")
    print("omaCortex is recording. Everything stays in", c.cfg.paths.data_dir)
    print("Pause any time with `omacortex pause` or the bar button.")
    return 0


def cmd_disable(args) -> int:
    c = _cortex()
    c.state.revoke()
    _poke(c.cfg, "refresh")
    print("omaCortex stopped recording. Your history is kept; `omacortex forget --all` deletes it.")
    return 0


def cmd_pause(args) -> int:
    c = _cortex()
    seconds = parse_duration(args.duration) if args.duration else None
    c.state.pause(seconds)
    _poke(c.cfg, "refresh")
    print("paused" + (f" for {human_duration(seconds)}" if seconds else " until you resume"))
    return 0


def cmd_resume(args) -> int:
    c = _cortex()
    c.state.resume()
    _poke(c.cfg, "refresh")
    print("recording" if c.state.read().consent else "resumed, but not enabled: run `omacortex enable`")
    return 0


def cmd_toggle(args) -> int:
    c = _cortex()
    rec = c.state.read()
    if not rec.consent:
        return cmd_enable(args)
    return cmd_resume(args) if rec.paused else cmd_pause(argparse.Namespace(duration=None))


def cmd_now(args) -> int:
    data = _cortex().now(args.minutes)
    if args.json:
        _print_json(data)
        return 0
    cur = data["current"]
    if cur:
        print(f"Now: {cur['app']}: {cur['title']} (for {human_duration(cur['seconds'])})")
    else:
        print("Now: nothing focused" + ("" if data["recording"] else " (not recording)"))
    for m in data["playing"]:
        print(f"Playing: {m['title']} [{m['app']}]")
    if data["recent_sessions"]:
        print(f"\nLast {args.minutes} minutes:")
        for s in data["recent_sessions"]:
            print(
                f"  {clock(s['started_at'])}  {human_duration(s['active_seconds']):>6}  {s['app']}: {s['title']}"
            )
    for n in data["recent_notes"]:
        print(f"Note {clock(n['started_at'])}: {n['title']}")
    return 0


def cmd_timeline(args) -> int:
    c = _cortex()
    rows = c.timeline(
        start=parse_when(args.since),
        end=parse_when(args.until),
        kinds=args.kind,
        min_seconds=args.min_seconds,
        limit=args.limit,
    )
    if args.json:
        _print_json(rows)
    else:
        for r in rows:
            print(_row_line(r))
    return 0


def cmd_sessions(args) -> int:
    rows = _cortex().sessions(start=parse_when(args.since), end=parse_when(args.until), limit=args.limit)
    if args.json:
        _print_json(rows)
        return 0
    for s in rows:
        extra = f"  (+{s['interruptions']} detours)" if s["interruptions"] else ""
        print(
            f"{clock(s['started_at'])}-{clock(s['ended_at'])}  {human_duration(s['active_seconds']):>6}  {s['app']}: {s['title']}{extra}"
        )
    return 0


def cmd_search(args) -> int:
    rows = _cortex().search(
        " ".join(args.query),
        start=parse_when(args.since),
        kinds=args.kind,
        limit=args.limit,
    )
    if args.json:
        _print_json(rows)
    else:
        for r in rows:
            print(_row_line(r))
        if not rows:
            print("no matches")
    return 0


def cmd_usage(args) -> int:
    rows = _cortex().usage(start=parse_when(args.since), end=parse_when(args.until), limit=args.limit)
    if args.json:
        _print_json(rows)
        return 0
    for u in rows:
        print(f"{human_duration(u['seconds']):>7}  {u['app'] or '(unknown)'}")
    return 0


def cmd_note(args) -> int:
    from omacortex.recorder import Recorder

    c = _cortex()
    note_id = Recorder(c.store, c.privacy, c.state).note(" ".join(args.text))
    print("noted" if note_id else "empty note ignored")
    return 0 if note_id else 1


def cmd_forget(args) -> int:
    c = _cortex()
    now = time.time()
    if args.all:
        scope = "everything omaCortex has recorded"
        kwargs = {"everything": True}
    elif args.last:
        scope = f"the last {human_duration(parse_duration(args.last))}"
        kwargs = {"start": now - parse_duration(args.last)}
    elif args.since or args.until or args.app:
        kwargs = {
            "start": parse_when(args.since),
            "end": parse_when(args.until),
            "app": args.app,
        }
        scope = "matching activity"
    else:
        print(
            "say what to forget: --last 15m, --since/--until, --app NAME, or --all",
            file=sys.stderr,
        )
        return 2
    if not args.yes:
        if not sys.stdin.isatty():
            print("refusing without --yes when not interactive", file=sys.stderr)
            return 2
        if input(f"Delete {scope}? This cannot be undone. [y/N] ").strip().lower() not in ("y", "yes"):
            print("kept")
            return 1
    count = c.store.forget(**kwargs)
    print(f"forgot {count} row(s)")
    return 0


def cmd_summarize(args) -> int:
    c = _cortex()
    result = c.summary(args.day, use_model=not args.no_model, refresh=args.refresh)
    if args.json:
        _print_json(result)
        return 0
    print(result["text"])
    if result.get("model"):
        print(f"\n({'cached, ' if result.get('cached') else ''}{result['model']})")
    elif result.get("model_error"):
        print(f"\n(no local model: {result['model_error']})", file=sys.stderr)
    return 0


def cmd_ask(args) -> int:
    from omacortex.llm import ModelError

    try:
        result = _cortex().ask(" ".join(args.question))
    except ModelError as exc:
        print(f"no local model: {exc}", file=sys.stderr)
        return 1
    if args.json:
        _print_json(result)
    else:
        print(result["answer"])
    return 0


def cmd_mcp(args) -> int:
    from omacortex.mcp_server import serve

    return serve(_cortex())


def cmd_panel_json(args) -> int:
    print(json.dumps(_cortex().panel(), ensure_ascii=False, default=str))
    return 0


def cmd_mark(args) -> int:
    from omacortex.daemon import send_control

    try:
        print(json.dumps(send_control(load_config(), args.what)))
    except (OSError, ValueError) as exc:
        print(f"daemon not reachable: {exc}", file=sys.stderr)
        return 1
    return 0


def cmd_export(args) -> int:
    c = _cortex()
    for row in c.store.export():
        row["started_at"] = iso(row["started_at"])
        row["ended_at"] = iso(row["ended_at"])
        print(json.dumps(row, ensure_ascii=False))
    return 0


def cmd_config(args) -> int:
    cfg = load_config()
    if args.init:
        if cfg.paths.config_file.exists():
            print(f"{cfg.paths.config_file} already exists", file=sys.stderr)
            return 1
        cfg.paths.config_dir.mkdir(parents=True, exist_ok=True)
        cfg.paths.config_file.write_text(EXAMPLE_CONFIG, encoding="utf-8")
        print(f"wrote {cfg.paths.config_file}")
        return 0
    if args.path:
        print(cfg.paths.config_file)
        return 0
    _print_json(cfg.data)
    return 0


def cmd_doctor(args) -> int:
    from omacortex import llm
    from omacortex.collectors import hyprland, media
    from omacortex.privacy import Privacy

    cfg = load_config()
    checks: list[tuple[str, bool, str]] = []
    checks.append(("config", not cfg.error, cfg.error or str(cfg.paths.config_file)))
    bad = Privacy.from_config(cfg).bad_patterns
    checks.append(("privacy patterns", not bad, ", ".join(bad) or "all valid"))
    d = hyprland.socket_dir()
    checks.append(
        (
            "hyprland socket",
            d is not None,
            "found" if d else "no running Hyprland instance",
        )
    )
    has_busctl = bool(shutil.which("busctl"))
    players = media.list_players() if has_busctl else []
    checks.append(
        (
            "busctl (media)",
            has_busctl,
            f"{len(players)} player(s) on the bus" if has_busctl else "missing",
        )
    )
    try:
        model = llm.resolve(cfg)
        checks.append(("local model", True, f"{model.model} at {model.endpoint}"))
    except llm.ModelError as exc:
        checks.append(("local model", False, f"optional: {exc}"))
    c = _cortex()
    s = c.status()
    checks.append(
        (
            "consent",
            s["consent"],
            "enabled" if s["consent"] else "run `omacortex enable`",
        )
    )
    checks.append(("daemon", s["daemon"], "running" if s["daemon"] else "not running"))
    perms = oct(c.cfg.paths.db.stat().st_mode & 0o777)
    checks.append(("database permissions", perms == "0o600", f"{c.cfg.paths.db} {perms}"))
    if args.json:
        _print_json([{"check": n, "ok": ok, "detail": d} for n, ok, d in checks])
    else:
        for name, ok, detail in checks:
            print(f"{'ok ' if ok else '!! '} {name:<22} {detail}")
    return 0


# -- parser --------------------------------------------------------------------


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="omacortex",
        description="A private, local memory of what you do on your Omarchy desktop.",
    )
    p.add_argument("--version", action="version", version=f"omacortex {__version__}")
    sub = p.add_subparsers(dest="command", required=True, metavar="COMMAND")

    def add(name, func, help_text):
        sp = sub.add_parser(name, help=help_text, description=help_text)
        sp.set_defaults(func=func)
        return sp

    def json_flag(sp):
        sp.add_argument("--json", action="store_true", help="machine-readable output")

    sp = add(
        "daemon",
        cmd_daemon,
        "run the capture daemon (the shell plugin starts it for you)",
    )
    sp.add_argument(
        "--if-not-running",
        action="store_true",
        help="exit quietly if one is already running",
    )
    json_flag(add("status", cmd_status, "show whether omaCortex is recording"))
    add("enable", cmd_enable, "give consent and start recording")
    add("disable", cmd_disable, "stop recording (keeps history)")
    sp = add("pause", cmd_pause, "pause recording, optionally for a while (15m, 1h)")
    sp.add_argument("duration", nargs="?")
    add("resume", cmd_resume, "resume recording")
    add("toggle", cmd_toggle, "pause or resume")

    sp = add("now", cmd_now, "what you are doing and were just doing")
    sp.add_argument("--minutes", type=int, default=30)
    json_flag(sp)

    sp = add("timeline", cmd_timeline, "raw activity in a time range")
    sp.add_argument("--since", default="1h")
    sp.add_argument("--until")
    sp.add_argument("--kind", action="append", choices=["focus", "media", "file", "note", "marker"])
    sp.add_argument("--min-seconds", type=float, default=0)
    sp.add_argument("--limit", type=int, default=200)
    json_flag(sp)

    sp = add("sessions", cmd_sessions, "focus grouped into sessions")
    sp.add_argument("--since", default="today")
    sp.add_argument("--until")
    sp.add_argument("--limit", type=int, default=50)
    json_flag(sp)

    sp = add("search", cmd_search, "full-text search over everything recorded")
    sp.add_argument("query", nargs="+")
    sp.add_argument("--since")
    sp.add_argument("--kind", action="append", choices=["focus", "media", "file", "note", "marker"])
    sp.add_argument("--limit", type=int, default=50)
    json_flag(sp)

    sp = add("usage", cmd_usage, "time per app")
    sp.add_argument("--since", default="today")
    sp.add_argument("--until")
    sp.add_argument("--limit", type=int, default=25)
    json_flag(sp)

    sp = add("note", cmd_note, "add a note or checkpoint to your timeline")
    sp.add_argument("text", nargs="+")

    sp = add("forget", cmd_forget, "delete recorded activity")
    sp.add_argument("--last", help="the last 15m, 2h, ...")
    sp.add_argument("--since")
    sp.add_argument("--until")
    sp.add_argument("--app", help="everything from one app (window class)")
    sp.add_argument("--all", action="store_true", help="everything")
    sp.add_argument("--yes", action="store_true", help="do not ask")

    sp = add("summarize", cmd_summarize, "summarize a day with your local model")
    sp.add_argument("day", nargs="?", default="today")
    sp.add_argument("--no-model", action="store_true", help="just print the digest")
    sp.add_argument("--refresh", action="store_true", help="ignore the cached summary")
    json_flag(sp)

    sp = add("ask", cmd_ask, "ask your local model about your activity")
    sp.add_argument("question", nargs="+")
    json_flag(sp)

    add("mcp", cmd_mcp, "run the MCP server on stdio for agents")
    add("panel-json", cmd_panel_json, "data for the shell panel (internal)")
    sp = add(
        "mark",
        cmd_mark,
        "tell the daemon you are away or back (used by the shell plugin)",
    )
    sp.add_argument("what", choices=["away", "back", "refresh", "ping"])
    add("export", cmd_export, "print everything as JSON lines")
    sp = add("config", cmd_config, "show the effective configuration")
    sp.add_argument("--init", action="store_true", help="write a commented config file")
    sp.add_argument("--path", action="store_true", help="print the config file path")
    json_flag(add("doctor", cmd_doctor, "check sensors, model and permissions"))
    return p


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        return int(args.func(args) or 0)
    except ValueError as exc:
        print(f"omacortex: {exc}", file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        return 130
