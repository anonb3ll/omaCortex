from omacortex.privacy import Privacy
from omacortex.recorder import Recorder, Track, Window
from omacortex.state import State
from omacortex.store import Store


def make(paths, clock, consent=True):
    state = State(paths, clock=clock)
    if consent:
        state.grant()
    store = Store(":memory:", clock=clock)
    privacy = Privacy(exclude_apps=["1password"], exclude_titles=[r"(?i)incognito"])
    return (
        Recorder(store, privacy, state, min_span_seconds=2, clock=clock),
        store,
        state,
    )


def focus_rows(store):
    return [(r["app"], r["title"], r["seconds"]) for r in store.timeline(start=0, kinds=["focus"])]


def test_nothing_is_recorded_without_consent(paths, clock):
    rec, store, _ = make(paths, clock, consent=False)
    rec.focus(Window("kitty", "nvim"))
    clock.tick(30)
    rec.media([Track("mpv", "A film")])
    assert store.timeline(start=0) == []


def test_focus_changes_produce_spans(paths, clock):
    rec, store, _ = make(paths, clock)
    rec.focus(Window("kitty", "nvim notes.md"))
    clock.tick(30)
    rec.focus(Window("kitty", "nvim notes.md"))  # same window: no new span
    clock.tick(30)
    rec.focus(Window("chromium", "Docs"))
    clock.tick(1)
    rec.focus(Window("kitty", "nvim notes.md"))  # chromium for 1s is dropped
    clock.tick(5)
    rec.shutdown()
    assert focus_rows(store) == [
        ("kitty", "nvim notes.md", 60),
        ("kitty", "nvim notes.md", 5),
    ]


def test_excluded_windows_leave_a_gap(paths, clock):
    rec, store, _ = make(paths, clock)
    rec.focus(Window("kitty", "shell"))
    clock.tick(10)
    rec.focus(Window("1Password", "Vault"))
    clock.tick(50)
    rec.focus(Window("chromium", "Docs - Chromium (Incognito)"))
    clock.tick(50)
    rec.focus(Window("kitty", "shell"))
    clock.tick(10)
    rec.shutdown()
    rows = focus_rows(store)
    assert rows == [("kitty", "shell", 10), ("kitty", "shell", 10)]
    assert store.search("Vault") == [] and store.search("Incognito") == []


def test_pause_closes_spans_and_resume_reopens_current_window(paths, clock):
    rec, store, state = make(paths, clock)
    rec.focus(Window("kitty", "shell"))
    rec.media([Track("spotify", "Song", "Artist")])
    clock.tick(20)
    state.pause()
    rec.refresh_state()
    rec.focus(Window("chromium", "while paused"))
    clock.tick(100)
    state.resume()
    rec.refresh_state()
    clock.tick(10)
    rec.shutdown()
    assert focus_rows(store) == [
        ("kitty", "shell", 20),
        ("chromium", "while paused", 10),
    ]
    markers = [r["title"] for r in store.timeline(start=0, kinds=["marker"])]
    assert markers == ["paused", "resumed"]
    media = store.timeline(start=0, kinds=["media"])
    assert len(media) == 1 and media[0]["seconds"] == 20


def test_timed_pause_expires(paths, clock):
    rec, store, state = make(paths, clock)
    state.pause(60)
    assert rec.refresh_state() is False
    clock.tick(61)
    assert rec.refresh_state() is True


def test_away_ends_focus_but_not_media(paths, clock):
    rec, store, _ = make(paths, clock)
    rec.focus(Window("mpv", "film.mkv"))
    rec.media([Track("mpv", "A film")])
    clock.tick(60)
    rec.set_idle(True)
    rec.media([Track("mpv", "A film")])
    clock.tick(600)
    rec.set_idle(False)
    clock.tick(30)
    rec.media([])
    rec.shutdown()
    assert focus_rows(store) == [("mpv", "film.mkv", 60), ("mpv", "film.mkv", 30)]
    media = store.timeline(start=0, kinds=["media"])
    assert [(m["title"], m["seconds"]) for m in media] == [("A film", 690)]


def test_media_tracks_change(paths, clock):
    rec, store, _ = make(paths, clock)
    rec.media([Track("spotify", "One", "A")])
    clock.tick(180)
    rec.media(
        [
            Track("spotify", "Two", "A"),
            Track("chromium", "Talk", url="https://v.example/watch?v=1"),
        ]
    )
    clock.tick(60)
    rec.media([])
    titles = [(m["title"], m["seconds"], m["detail"]) for m in store.timeline(start=0, kinds=["media"])]
    assert titles == [
        ("One", 180, "A"),
        ("Two", 60, "A"),
        ("Talk", 60, "https://v.example/watch"),
    ]


def test_files_respect_exclusions(paths, clock, tmp_path):
    rec, store, _ = make(paths, clock)
    rec.privacy = Privacy(exclude_paths=[str(tmp_path / "secret")])
    assert rec.file_opened(str(tmp_path / "docs/plan.md"), "org.gnome.TextEditor")
    assert not rec.file_opened(str(tmp_path / "secret/x.md"))
    assert not rec.file_opened(str(tmp_path / "id_rsa"))
    assert [r["title"] for r in store.timeline(start=0, kinds=["file"])] == ["plan.md"]


def test_notes_are_cleaned_and_kept_while_paused(paths, clock):
    rec, store, state = make(paths, clock)
    state.pause()
    rec.refresh_state()
    assert rec.note("ping someone@example.com about the draft")
    assert rec.note("   ") is None
    assert store.timeline(start=0, kinds=["note"])[0]["title"] == "ping [email] about the draft"
