//! Turns sensor readings into spans in the store.
//!
//! Sensors report *what is true now* ("this window is focused", "these tracks
//! are playing"); the recorder works out which spans to open and close. Every
//! reading passes the privacy guard and the consent/pause state first, so a
//! sensor never writes to the store directly.

use crate::privacy::Privacy;
use crate::state::State;
use crate::store::Store;
use crate::timeutil::Clock;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Window {
    pub app: String,
    pub title: String,
    pub workspace: String,
}

impl Window {
    pub fn new(app: &str, title: &str, workspace: &str) -> Self {
        Self {
            app: app.into(),
            title: title.into(),
            workspace: workspace.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Track {
    pub player: String,
    pub title: String,
    pub artist: String,
    pub url: String,
}

/// One visit to a web page, as a browser extension reports it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Page {
    pub url: String,
    pub title: String,
    pub site: String,
    pub author: String,
    /// "video", "article" or "page".
    pub content: String,
    /// Foreground seconds so far.
    pub seconds: f64,
    /// When the visit started (epoch seconds). The same visit keeps the same start.
    pub at: Option<f64>,
    /// Which extension reported it.
    pub source: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PageOutcome {
    Stored(i64),
    Skipped(&'static str),
}

type TrackKey = (String, String, String);

impl Track {
    fn key(&self) -> TrackKey {
        (self.player.clone(), self.title.clone(), self.artist.clone())
    }
}

#[derive(Default)]
struct Inner {
    focus_id: Option<i64>,
    focus_key: Option<(String, String)>,
    /// Last window reported, even while away or paused.
    window: Option<Window>,
    media: BTreeMap<TrackKey, i64>,
    idle: bool,
    recording: bool,
}

pub struct Recorder {
    pub store: Arc<Store>,
    pub privacy: Privacy,
    state: State,
    min_span: f64,
    clock: Clock,
    inner: Mutex<Inner>,
}

impl Recorder {
    pub fn new(
        store: Arc<Store>,
        privacy: Privacy,
        state: State,
        min_span: f64,
        clock: Clock,
    ) -> Self {
        let recording = state.read().recording();
        Self {
            store,
            privacy,
            state,
            min_span,
            clock,
            inner: Mutex::new(Inner {
                recording,
                ..Default::default()
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn at(&self, at: Option<f64>) -> f64 {
        at.unwrap_or_else(|| (self.clock)())
    }

    pub fn recording(&self) -> bool {
        let i = self.lock();
        i.recording && !i.idle
    }

    /// Re-read consent/pause; close everything when recording stops.
    pub fn refresh_state(&self, at: Option<f64>) -> bool {
        let at = self.at(at);
        let now_recording = self.state.read().recording();
        let mut i = self.lock();
        if now_recording == i.recording {
            return now_recording;
        }
        i.recording = now_recording;
        if !now_recording {
            self.close_focus(&mut i, at);
            self.close_media(&mut i, at);
            let _ = self
                .store
                .add_event("marker", "omacortex", "", "paused", "", Some(at));
        } else {
            let _ = self
                .store
                .add_event("marker", "omacortex", "", "resumed", "", Some(at));
            if let (Some(w), false) = (i.window.clone(), i.idle) {
                self.open_focus(&mut i, &w, at);
            }
        }
        now_recording
    }

    pub fn set_idle(&self, idle: bool, at: Option<f64>) {
        let at = self.at(at);
        let mut i = self.lock();
        if idle == i.idle {
            return;
        }
        i.idle = idle;
        if idle {
            // Focus ends when you walk away; media keeps playing (you may be watching).
            self.close_focus(&mut i, at);
            if i.recording {
                let _ = self
                    .store
                    .add_event("marker", "omacortex", "", "away", "", Some(at));
            }
        } else if i.recording {
            let _ = self
                .store
                .add_event("marker", "omacortex", "", "back", "", Some(at));
            if let Some(w) = i.window.clone() {
                self.open_focus(&mut i, &w, at);
            }
        }
    }

    pub fn focus(&self, window: Option<Window>, at: Option<f64>) {
        let at = self.at(at);
        let mut i = self.lock();
        i.window = window.clone();
        if !i.recording || i.idle {
            return;
        }
        match window {
            Some(w) if !(w.app.is_empty() && w.title.is_empty()) => {
                if self.privacy.window_excluded(&w.app, &w.title, &w.workspace) {
                    self.close_focus(&mut i, at);
                } else {
                    self.open_focus(&mut i, &w, at);
                }
            }
            _ => self.close_focus(&mut i, at),
        }
    }

    pub fn media(&self, tracks: &[Track], at: Option<f64>) {
        let at = self.at(at);
        let mut i = self.lock();
        if !i.recording {
            self.close_media(&mut i, at);
            return;
        }
        let wanted: BTreeMap<TrackKey, &Track> = tracks
            .iter()
            .filter(|t| !t.title.is_empty() && !self.privacy.title_excluded(&t.title))
            .map(|t| (t.key(), t))
            .collect();
        let gone: Vec<TrackKey> = i
            .media
            .keys()
            .filter(|k| !wanted.contains_key(*k))
            .cloned()
            .collect();
        for key in gone {
            if let Some(id) = i.media.remove(&key) {
                let _ = self.store.close_span(id, Some(at), self.min_span);
            }
        }
        for (key, t) in wanted {
            if i.media.contains_key(&key) {
                continue;
            }
            let detail: Vec<&str> = [t.artist.as_str(), t.url.as_str()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect();
            if let Ok(id) = self.store.open_span(
                "media",
                "mpris",
                &self.privacy.clean(&t.player),
                &self.privacy.clean(&t.title),
                &self.privacy.clean(&detail.join(" · ")),
                Some(at),
            ) {
                i.media.insert(key, id);
            }
        }
    }

    pub fn file_opened(&self, path: &str, app: &str, at: Option<f64>) -> bool {
        let at = self.at(at);
        let i = self.lock();
        if !i.recording || self.privacy.path_excluded(path) {
            return false;
        }
        let name = path.rsplit('/').next().unwrap_or(path);
        self.store
            .add_event(
                "file",
                "recent-files",
                &self.privacy.clean(app),
                &self.privacy.clean(name),
                &self.privacy.clean(path),
                Some(at),
            )
            .is_ok()
    }

    /// A page visit reported by a browser extension. Unlike the sensors, the
    /// reporter is another program, so nothing it sends is trusted: the
    /// address must be a web page on a host that is not excluded, the title
    /// must pass the same rules as a window title, and every field is cleaned.
    pub fn page(&self, page: &Page, min_seconds: f64) -> PageOutcome {
        if !self.state.read().recording() {
            return PageOutcome::Skipped("paused");
        }
        let Some(host) = crate::privacy::host_of(&page.url) else {
            return PageOutcome::Skipped("not a web page");
        };
        if self.privacy.host_excluded(&host) || self.privacy.title_excluded(&page.title) {
            return PageOutcome::Skipped("excluded");
        }
        if !page.seconds.is_finite() || page.seconds < min_seconds {
            return PageOutcome::Skipped("too short");
        }
        let now = (self.clock)();
        // A visit cannot start in the future or last longer than a day.
        let seconds = page.seconds.min(86400.0);
        let at = match page.at {
            Some(at) if at.is_finite() && at > 0.0 && at <= now + 60.0 => at.min(now),
            _ => now - seconds,
        };
        let title = self.privacy.clean(&page.title);
        let Some(url) = self.privacy.clean_url(&page.url) else {
            return PageOutcome::Skipped("not a web page");
        };
        if title.is_empty() {
            return PageOutcome::Skipped("no title");
        }
        let content = match page.content.as_str() {
            "video" | "article" => page.content.as_str(),
            _ => "page",
        };
        let source = format!(
            "browser:{}",
            crate::privacy::bounded(&page.source, 24)
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect::<String>()
        );
        let visit = crate::store::PageVisit {
            source: source.trim_end_matches(':'),
            host: host.strip_prefix("www.").unwrap_or(&host),
            title: &title,
            url: &url,
            site: &self.privacy.clean(&crate::privacy::bounded(&page.site, 80)),
            author: &self
                .privacy
                .clean(&crate::privacy::bounded(&page.author, 120)),
            content,
            at,
            seconds,
        };
        match self.store.upsert_page(&visit) {
            Ok((id, _)) => PageOutcome::Stored(id),
            Err(_) => PageOutcome::Skipped("store error"),
        }
    }

    /// A note is something you chose to record, so it is kept even while paused.
    pub fn note(&self, text: &str, source: &str, at: Option<f64>) -> Option<i64> {
        let text = self.privacy.clean(text);
        if text.is_empty() {
            return None;
        }
        self.store
            .add_event("note", source, "", &text, "", Some(self.at(at)))
            .ok()
    }

    pub fn shutdown(&self, at: Option<f64>) {
        let at = self.at(at);
        let mut i = self.lock();
        self.close_focus(&mut i, at);
        self.close_media(&mut i, at);
    }

    fn open_focus(&self, i: &mut Inner, w: &Window, at: f64) {
        let key = (self.privacy.clean(&w.app), self.privacy.clean(&w.title));
        if i.focus_id.is_some() && i.focus_key.as_ref() == Some(&key) {
            return;
        }
        self.close_focus(i, at);
        if let Ok(id) =
            self.store
                .open_span("focus", "hyprland", &key.0, &key.1, &w.workspace, Some(at))
        {
            i.focus_id = Some(id);
            i.focus_key = Some(key);
        }
    }

    fn close_focus(&self, i: &mut Inner, at: f64) {
        if let Some(id) = i.focus_id.take() {
            let _ = self.store.close_span(id, Some(at), self.min_span);
        }
        i.focus_key = None;
    }

    fn close_media(&self, i: &mut Inner, at: f64) {
        for (_, id) in std::mem::take(&mut i.media) {
            let _ = self.store.close_span(id, Some(at), self.min_span);
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::config::Paths;
    use crate::store::Query;
    use crate::store::tests::TestClock;

    pub fn temp_paths(tag: &str) -> Paths {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let base = std::env::temp_dir().join(format!(
            "oc-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&base);
        let p = Paths {
            config_dir: base.join("c"),
            data_dir: base.join("d"),
            state_dir: base.join("s"),
        };
        p.ensure().unwrap();
        p
    }

    fn make(consent: bool) -> (Recorder, Arc<Store>, State, TestClock) {
        let c = TestClock::new();
        let state = State::new(temp_paths("rec"), c.clock());
        if consent {
            state.grant().unwrap();
        }
        let store = Arc::new(Store::memory(c.clock()).unwrap());
        let privacy = Privacy::new(
            true,
            &["1password".into()],
            &["(?i)incognito".into()],
            &[],
            &[],
        );
        (
            Recorder::new(store.clone(), privacy, state.clone(), 2.0, c.clock()),
            store,
            state,
            c,
        )
    }

    fn rows(store: &Store, kind: &str) -> Vec<crate::store::Row> {
        let kinds = [kind.to_string()];
        store
            .timeline(&Query {
                start: Some(0.0),
                kinds: &kinds,
                limit: 100,
                ..Default::default()
            })
            .unwrap()
    }

    fn focus(store: &Store) -> Vec<(String, String, f64)> {
        rows(store, "focus")
            .into_iter()
            .map(|r| (r.app, r.title, r.seconds))
            .collect()
    }

    fn f(a: &str, t: &str, s: f64) -> (String, String, f64) {
        (a.into(), t.into(), s)
    }

    #[test]
    fn nothing_without_consent() {
        let (rec, store, _, c) = make(false);
        rec.focus(Some(Window::new("kitty", "nvim", "")), None);
        c.tick(30.0);
        rec.media(
            &[Track {
                player: "mpv".into(),
                title: "A film".into(),
                ..Default::default()
            }],
            None,
        );
        assert!(store.export().unwrap().is_empty());
    }

    #[test]
    fn focus_changes_produce_spans() {
        let (rec, store, _, c) = make(true);
        rec.focus(Some(Window::new("kitty", "nvim notes.md", "")), None);
        c.tick(30.0);
        rec.focus(Some(Window::new("kitty", "nvim notes.md", "")), None);
        c.tick(30.0);
        rec.focus(Some(Window::new("chromium", "Docs", "")), None);
        c.tick(1.0);
        rec.focus(Some(Window::new("kitty", "nvim notes.md", "")), None);
        c.tick(5.0);
        rec.shutdown(None);
        assert_eq!(
            focus(&store),
            vec![
                f("kitty", "nvim notes.md", 60.0),
                f("kitty", "nvim notes.md", 5.0)
            ]
        );
    }

    #[test]
    fn excluded_windows_leave_a_gap() {
        let (rec, store, _, c) = make(true);
        rec.focus(Some(Window::new("kitty", "shell", "")), None);
        c.tick(10.0);
        rec.focus(Some(Window::new("1Password", "Vault", "")), None);
        c.tick(50.0);
        rec.focus(
            Some(Window::new("chromium", "Docs - Chromium (Incognito)", "")),
            None,
        );
        c.tick(50.0);
        rec.focus(Some(Window::new("kitty", "shell", "")), None);
        c.tick(10.0);
        rec.shutdown(None);
        assert_eq!(
            focus(&store),
            vec![f("kitty", "shell", 10.0), f("kitty", "shell", 10.0)]
        );
    }

    #[test]
    fn pause_closes_spans_and_resume_reopens() {
        let (rec, store, state, c) = make(true);
        rec.focus(Some(Window::new("kitty", "shell", "")), None);
        rec.media(
            &[Track {
                player: "spotify".into(),
                title: "Song".into(),
                artist: "A".into(),
                url: String::new(),
            }],
            None,
        );
        c.tick(20.0);
        state.pause(None).unwrap();
        rec.refresh_state(None);
        rec.focus(Some(Window::new("chromium", "while paused", "")), None);
        c.tick(100.0);
        state.resume().unwrap();
        rec.refresh_state(None);
        c.tick(10.0);
        rec.shutdown(None);
        assert_eq!(
            focus(&store),
            vec![
                f("kitty", "shell", 20.0),
                f("chromium", "while paused", 10.0)
            ]
        );
        let markers: Vec<_> = rows(&store, "marker")
            .into_iter()
            .map(|r| r.title)
            .collect();
        assert_eq!(markers, vec!["paused", "resumed"]);
        let media = rows(&store, "media");
        assert_eq!((media.len(), media[0].seconds), (1, 20.0));
    }

    #[test]
    fn timed_pause_expires() {
        let (rec, _, state, c) = make(true);
        state.pause(Some(60.0)).unwrap();
        assert!(!rec.refresh_state(None));
        c.tick(61.0);
        assert!(rec.refresh_state(None));
    }

    #[test]
    fn away_ends_focus_but_not_media() {
        let (rec, store, _, c) = make(true);
        let film = [Track {
            player: "mpv".into(),
            title: "A film".into(),
            ..Default::default()
        }];
        rec.focus(Some(Window::new("mpv", "film.mkv", "")), None);
        rec.media(&film, None);
        c.tick(60.0);
        rec.set_idle(true, None);
        rec.media(&film, None);
        c.tick(600.0);
        rec.set_idle(false, None);
        c.tick(30.0);
        rec.media(&[], None);
        rec.shutdown(None);
        assert_eq!(
            focus(&store),
            vec![f("mpv", "film.mkv", 60.0), f("mpv", "film.mkv", 30.0)]
        );
        let media = rows(&store, "media");
        assert_eq!(
            (media[0].title.as_str(), media[0].seconds),
            ("A film", 690.0)
        );
    }

    #[test]
    fn media_tracks_change() {
        let (rec, store, _, c) = make(true);
        let t = |p: &str, title: &str, a: &str, u: &str| Track {
            player: p.into(),
            title: title.into(),
            artist: a.into(),
            url: u.into(),
        };
        rec.media(&[t("spotify", "One", "A", "")], None);
        c.tick(180.0);
        rec.media(
            &[
                t("spotify", "Two", "A", ""),
                t("chromium", "Talk", "", "https://v.example/watch?v=1"),
            ],
            None,
        );
        c.tick(60.0);
        rec.media(&[], None);
        let mut got: Vec<_> = rows(&store, "media")
            .into_iter()
            .map(|r| (r.title, r.seconds, r.detail))
            .collect();
        got.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            got,
            vec![
                ("One".into(), 180.0, "A".into()),
                ("Talk".into(), 60.0, "https://v.example/watch".into()),
                ("Two".into(), 60.0, "A".into()),
            ]
        );
    }

    #[test]
    fn files_respect_exclusions() {
        let (mut rec, store, _, _) = make(true);
        rec.privacy = Privacy::new(true, &[], &[], &[], &["/tmp/oc-secret".into()]);
        assert!(rec.file_opened("/tmp/oc-docs/plan.md", "Text Editor", None));
        assert!(!rec.file_opened("/tmp/oc-secret/x.md", "", None));
        assert!(!rec.file_opened("/tmp/oc-docs/id_rsa", "", None));
        let titles: Vec<_> = rows(&store, "file").into_iter().map(|r| r.title).collect();
        assert_eq!(titles, vec!["plan.md"]);
    }

    fn page(url: &str, title: &str, seconds: f64, at: Option<f64>) -> Page {
        Page {
            url: url.into(),
            title: title.into(),
            site: "YouTube".into(),
            author: "A Channel".into(),
            content: "video".into(),
            seconds,
            at,
            source: "companion".into(),
        }
    }

    #[test]
    fn pages_are_guarded_cleaned_and_lengthened() {
        let (mut rec, store, state, c) = make(true);
        rec.privacy = Privacy::from_settings(&crate::config::Privacy::default());
        let t0 = c.now();
        c.tick(600.0);
        let watch = "https://www.youtube.com/watch?v=abc123XYZ_-&t=10s";
        let PageOutcome::Stored(id) =
            rec.page(&page(watch, "Async Rust in practice", 60.0, Some(t0)), 10.0)
        else {
            panic!("not stored")
        };
        // The same visit reported again with more time lengthens the row.
        assert_eq!(
            rec.page(
                &page(watch, "Async Rust in practice", 540.0, Some(t0)),
                10.0
            ),
            PageOutcome::Stored(id)
        );
        let rows = rows(&store, "page");
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].app.as_str(), rows[0].seconds),
            ("youtube.com", 540.0)
        );
        assert_eq!(rows[0].source, "browser:companion");
        assert_eq!(
            rows[0].detail,
            "YouTube · A Channel · https://www.youtube.com/watch?v=abc123XYZ_-"
        );

        for (url, title, secs, why) in [
            ("https://accounts.google.com/x", "Account", 60.0, "excluded"),
            (
                "https://example.com/",
                "Sign in to Example",
                60.0,
                "excluded",
            ),
            ("https://example.com/", "Fine", 3.0, "too short"),
            ("chrome://settings", "Settings", 60.0, "not a web page"),
            ("https://example.com/", "  ", 60.0, "no title"),
            ("https://example.com/", "Fine", f64::NAN, "too short"),
        ] {
            assert_eq!(
                rec.page(&page(url, title, secs, None), 10.0),
                PageOutcome::Skipped(why),
                "{url} {title}"
            );
        }
        // A start time in the future is not believed.
        let PageOutcome::Stored(late) = rec.page(
            &page("https://example.com/a", "Later", 30.0, Some(c.now() + 9e6)),
            10.0,
        ) else {
            panic!("not stored")
        };
        assert!(store.get(late).unwrap().unwrap().started_at <= c.now());

        state.pause(None).unwrap();
        assert_eq!(
            rec.page(
                &page("https://example.com/b", "While paused", 60.0, None),
                10.0
            ),
            PageOutcome::Skipped("paused")
        );
        assert_eq!(super::tests::rows(&store, "page").len(), 2);
    }

    #[test]
    fn notes_are_cleaned_and_kept_while_paused() {
        let (rec, store, state, _) = make(true);
        state.pause(None).unwrap();
        rec.refresh_state(None);
        assert!(
            rec.note("ping someone@example.com about the draft", "cli", None)
                .is_some()
        );
        assert!(rec.note("   ", "cli", None).is_none());
        assert_eq!(
            rows(&store, "note")[0].title,
            "ping [email] about the draft"
        );
    }
}
