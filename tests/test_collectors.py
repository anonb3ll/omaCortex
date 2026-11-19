import json
import threading

from omacortex.collectors.files import FilesCollector, parse_xbel
from omacortex.collectors.hyprland import EventParser
from omacortex.collectors.media import parse_properties, player_label, track_from
from omacortex.recorder import Window


def test_hyprland_event_parser_tracks_focus_and_titles():
    p = EventParser()
    p.seed(Window("kitty", "shell", "1"))
    assert p.feed("workspacev2>>2,2") == (False, Window("kitty", "shell", "1"))
    p.feed("activewindowv2>>0xabc")
    changed, w = p.feed("activewindow>>chromium,Issue 12, comments - GitHub")
    assert changed and w == Window("chromium", "Issue 12, comments - GitHub", "2")
    # Title change of the focused window is a new reading...
    changed, w = p.feed("windowtitlev2>>0xabc,Pull 13 - GitHub")
    assert changed and w.title == "Pull 13 - GitHub"
    # ...but a background window's title change is not.
    assert p.feed("windowtitlev2>>0xdef,Other")[0] is False
    p.feed("activespecial>>special:scratch,DP-1")
    _, w = p.feed("activewindow>>kitty,scratch")
    assert w.workspace == "special:scratch"
    changed, w = p.feed("activewindow>>,")
    assert changed and w is None
    assert p.feed("garbage without separator") == (False, None)


def test_mpris_parsing():
    status = json.dumps({"type": "s", "data": "Playing"})
    meta = json.dumps(
        {
            "type": "a{sv}",
            "data": {
                "xesam:title": {"type": "s", "data": "A talk"},
                "xesam:artist": {"type": "as", "data": ["Speaker"]},
                "xesam:url": {"type": "s", "data": "https://video.example/watch?v=1"},
            },
        }
    )
    st, md = parse_properties(f"{status}\n{meta}\n")
    track = track_from("org.mpris.MediaPlayer2.chromium.instance42", st, md)
    assert track.player == "chromium" and track.title == "A talk" and track.artist == "Speaker"
    assert track_from("org.mpris.MediaPlayer2.mpv", "Paused", md) is None
    md_local = dict(md, **{"xesam:url": "file:///home/u/film.mkv"})
    assert track_from("org.mpris.MediaPlayer2.mpv", "Playing", md_local).url == ""
    assert parse_properties("") == ("", {})
    assert player_label("org.mpris.MediaPlayer2.spotify") == "spotify"


XBEL = """<?xml version="1.0" encoding="UTF-8"?>
<xbel version="1.0" xmlns:bookmark="http://www.freedesktop.org/standards/desktop-bookmarks"
      xmlns:mime="http://www.freedesktop.org/standards/shared-mime-info">
  <bookmark href="file:///home/u/docs/plan%20v2.md" added="2026-11-19T08:00:00Z"
            modified="2026-11-19T08:00:00Z" visited="2026-11-19T08:00:00Z">
    <info><metadata owner="http://freedesktop.org">
      <bookmark:applications>
        <bookmark:application name="Text Editor" exec="'gnome-text-editor %u'"
            modified="2026-11-19T09:30:00Z" count="2"/>
      </bookmark:applications>
    </metadata></info>
  </bookmark>
  <bookmark href="https://example.com/remote" added="2026-11-19T09:00:00Z"/>
  <bookmark href="file:///home/u/old.txt" added="2026-11-01T09:00:00Z" modified="2026-11-01T09:00:00Z"/>
</xbel>"""


def test_xbel_parsing():
    rows = parse_xbel(XBEL)
    assert [(p, a) for _, p, a in rows] == [
        ("/home/u/old.txt", ""),
        ("/home/u/docs/plan v2.md", "Text Editor"),
    ]
    assert parse_xbel("<not xml") == []


def test_files_collector_uses_watermark(tmp_path):
    xbel = tmp_path / "recently-used.xbel"
    xbel.write_text(XBEL)
    seen, mark = [], {"v": parse_xbel(XBEL)[0][0]}  # already saw old.txt
    c = FilesCollector(
        on_file=lambda path, app, ts: seen.append(path),
        watermark=lambda: mark["v"],
        set_watermark=lambda v: mark.__setitem__("v", v),
        stop=threading.Event(),
        path=xbel,
    )
    assert c.poll() == 1 and seen == ["/home/u/docs/plan v2.md"]
    assert c.poll() == 0  # unchanged file is not re-read
