//! Focused-window sensor over Hyprland's IPC sockets.
//!
//! Reads `.socket2.sock` (the event stream) and asks `.socket.sock` for the
//! active window at start and after each reconnect. Only the window class,
//! title and workspace name are used; nothing is read from the window itself.

use super::wait;
use crate::recorder::Window;
use serde_json::Value;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub fn socket_dir() -> Option<PathBuf> {
    let runtime = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/run/user/{}", unsafe { libc::getuid() }));
    let base = Path::new(&runtime).join("hypr");
    if let Ok(sig) = std::env::var("HYPRLAND_INSTANCE_SIGNATURE") {
        let dir = base.join(sig);
        if dir.join(".socket2.sock").exists() {
            return Some(dir);
        }
    }
    // Not inherited (e.g. a systemd unit started before Hyprland exported its
    // environment): use the newest live instance.
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&base)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|e| {
            let sock = e.path().join(".socket2.sock");
            let mtime = std::fs::metadata(&sock).ok()?.modified().ok()?;
            Some((mtime, e.path()))
        })
        .collect();
    found.sort_by_key(|f| std::cmp::Reverse(f.0));
    found.into_iter().next().map(|(_, p)| p)
}

pub fn request(dir: &Path, command: &str) -> std::io::Result<String> {
    let mut sock = UnixStream::connect(dir.join(".socket.sock"))?;
    sock.set_read_timeout(Some(Duration::from_secs(2)))?;
    sock.write_all(command.as_bytes())?;
    let mut buf = Vec::new();
    sock.take(4 * 1024 * 1024).read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

pub fn active_window(dir: &Path) -> Option<Window> {
    let v: Value = serde_json::from_str(&request(dir, "j/activewindow").ok()?).ok()?;
    let app = v["class"].as_str().unwrap_or_default();
    let title = v["title"].as_str().unwrap_or_default();
    if app.is_empty() && title.is_empty() {
        return None;
    }
    Some(Window::new(
        app,
        title,
        v["workspace"]["name"].as_str().unwrap_or_default(),
    ))
}

/// Folds Hyprland events into "the focused window is now X" readings.
///
/// ```text
/// activewindow>>CLASS,TITLE        focus moved (CLASS never contains a comma)
/// activewindowv2>>ADDRESS          address of the newly focused window
/// windowtitlev2>>ADDRESS,TITLE     a window's title changed
/// workspacev2>>ID,NAME             the active workspace changed
/// activespecial>>NAME,MONITOR      a special workspace was shown or hidden
/// closewindow>>ADDRESS             a window closed
/// screencast>>STATE,OWNER          sharing started (1) or stopped (0)
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventEffect {
    /// Nothing the recorder needs to hear.
    Ignore,
    /// The focused window changed and capture is allowed.
    Window,
    /// Screen sharing started (`true`) or stopped (`false`).
    Sharing(bool),
}

#[derive(Default)]
pub struct EventParser {
    address: String,
    pub window: Option<Window>,
    workspace: String,
    special: String,
    /// Number of observed shares that have not sent a stop event.
    active_shares: usize,
}

impl EventParser {
    pub fn seed(&mut self, window: Option<Window>) {
        self.address.clear();
        self.special.clear();
        self.workspace = window
            .as_ref()
            .map(|w| w.workspace.clone())
            .unwrap_or_default();
        self.window = window;
    }

    /// A window change is reported only while nothing is being shared.
    /// The focused window is still tracked during a share, so capture can
    /// resume on the same window when sharing stops.
    pub fn feed(&mut self, line: &str) -> EventEffect {
        let Some((name, data)) = line.trim_end_matches('\n').split_once(">>") else {
            return EventEffect::Ignore;
        };
        match name {
            "activewindow" => {
                let (app, title) = data.split_once(',').unwrap_or((data, ""));
                let ws = if self.special.is_empty() {
                    self.workspace.clone()
                } else {
                    self.special.clone()
                };
                self.window = if app.is_empty() && title.is_empty() {
                    None
                } else {
                    Some(Window::new(app, title, &ws))
                };
                self.visible(true)
            }
            "activewindowv2" => {
                self.address = data.trim().to_string();
                EventEffect::Ignore
            }
            "windowtitlev2" => {
                let (addr, title) = data.split_once(',').unwrap_or((data, ""));
                let changed = match &mut self.window {
                    Some(w) if addr == self.address && w.title != title => {
                        w.title = title.to_string();
                        true
                    }
                    _ => false,
                };
                self.visible(changed)
            }
            "workspacev2" => {
                self.workspace = data.split_once(',').map_or("", |(_, n)| n).to_string();
                EventEffect::Ignore
            }
            "activespecial" => {
                self.special = data.split_once(',').map_or(data, |(s, _)| s).to_string();
                EventEffect::Ignore
            }
            "closewindow" => {
                if data.trim() == self.address {
                    self.address.clear();
                }
                EventEffect::Ignore
            }
            "screencast" => self.screencast(data),
            _ => EventEffect::Ignore,
        }
    }

    fn visible(&self, changed: bool) -> EventEffect {
        if changed && !self.is_sharing() {
            EventEffect::Window
        } else {
            EventEffect::Ignore
        }
    }

    fn is_sharing(&self) -> bool {
        self.active_shares > 0
    }

    /// `screencast>>1,...` starts a share. `screencast>>0` (with or without
    /// a trailing owner) stops it. Anything else is ignored.
    fn screencast(&mut self, data: &str) -> EventEffect {
        let state = data.split(',').next().unwrap_or(data).trim();
        let was_sharing = self.is_sharing();
        match state {
            "1" => self.active_shares = self.active_shares.saturating_add(1),
            "0" => self.active_shares = self.active_shares.saturating_sub(1),
            _ => return EventEffect::Ignore,
        }
        let sharing = self.is_sharing();
        if sharing == was_sharing {
            return EventEffect::Ignore;
        }
        EventEffect::Sharing(sharing)
    }
}

pub fn run(on_window: impl Fn(Option<Window>), stop: &AtomicBool) {
    let mut backoff = 1.0_f64;
    let mut instance = None;
    let mut parser = EventParser::default();
    while !stop.load(Ordering::Relaxed) {
        match socket_dir() {
            None => eprintln!("omacortex: hyprland: no running instance found; retrying"),
            Some(dir) => {
                if instance.as_ref() != Some(&dir) {
                    parser = EventParser::default();
                    instance = Some(dir.clone());
                }
                // A socket disconnect does not end a share in this instance.
                match stream(&dir, &mut parser, &on_window, stop) {
                    Ok(()) => backoff = 1.0,
                    Err(e) => eprintln!("omacortex: hyprland: event socket: {e}"),
                }
            }
        }
        wait(stop, backoff);
        backoff = (backoff * 2.0).min(30.0);
    }
}

fn stream(
    dir: &Path,
    parser: &mut EventParser,
    on_window: &impl Fn(Option<Window>),
    stop: &AtomicBool,
) -> std::io::Result<()> {
    let sock = UnixStream::connect(dir.join(".socket2.sock"))?;
    sock.set_read_timeout(Some(Duration::from_secs(1)))?;
    parser.seed(active_window(dir));
    if !parser.is_sharing() {
        on_window(parser.window.clone());
    }
    let mut reader = BufReader::new(sock);
    let mut line = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    ErrorKind::UnexpectedEof,
                    "event socket closed",
                ));
            }
            Ok(_) if line.ends_with(b"\n") => {
                match parser.feed(&String::from_utf8_lossy(&line)) {
                    EventEffect::Window => on_window(parser.window.clone()),
                    EventEffect::Sharing(true) => {
                        eprintln!(
                            "omacortex: hyprland: screen sharing started; window capture paused"
                        );
                        on_window(None);
                    }
                    EventEffect::Sharing(false) => {
                        eprintln!(
                            "omacortex: hyprland: screen sharing stopped; window capture resumed"
                        );
                        parser.seed(active_window(dir));
                        on_window(parser.window.clone());
                    }
                    EventEffect::Ignore => {}
                }
                line.clear();
            }
            Ok(_) => {}
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => return Err(e),
        }
        if line.len() > 1024 * 1024 {
            line.clear();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_tracks_focus_and_titles() {
        let mut p = EventParser::default();
        p.seed(Some(Window::new("kitty", "shell", "1")));
        assert_eq!(p.feed("workspacev2>>2,2"), EventEffect::Ignore);
        p.feed("activewindowv2>>0xabc");
        assert_eq!(
            p.feed("activewindow>>chromium,Issue 12, comments - GitHub"),
            EventEffect::Window
        );
        assert_eq!(
            p.window,
            Some(Window::new("chromium", "Issue 12, comments - GitHub", "2"))
        );
        assert_eq!(
            p.feed("windowtitlev2>>0xabc,Pull 13 - GitHub"),
            EventEffect::Window
        );
        assert_eq!(p.window.as_ref().unwrap().title, "Pull 13 - GitHub");
        assert_eq!(p.feed("windowtitlev2>>0xdef,Other"), EventEffect::Ignore);
        p.feed("activespecial>>special:scratch,DP-1");
        p.feed("activewindow>>kitty,scratch");
        assert_eq!(p.window.as_ref().unwrap().workspace, "special:scratch");
        assert_eq!(p.feed("activewindow>>,"), EventEffect::Window);
        assert_eq!(p.window, None);
        assert_eq!(p.feed("garbage without separator"), EventEffect::Ignore);
    }

    #[test]
    fn screencast_pauses_capture_and_remembers_the_window() {
        let mut p = EventParser::default();
        p.seed(Some(Window::new("kitty", "shell", "1")));
        assert!(!p.is_sharing());

        assert_eq!(p.feed("screencast>>1,0"), EventEffect::Sharing(true));
        assert!(p.is_sharing());
        // A second client keeps capture paused after the first one stops.
        assert_eq!(p.feed("screencast>>1,1"), EventEffect::Ignore);
        assert_eq!(p.feed("screencast>>2,0"), EventEffect::Ignore);
        assert_eq!(p.feed("screencast>>0,1"), EventEffect::Ignore);
        assert!(p.is_sharing());

        // Focus still updates, but nothing is emitted while sharing.
        assert_eq!(p.feed("activewindowv2>>0xabc"), EventEffect::Ignore);
        assert_eq!(p.feed("windowtitlev2>>0xabc,notes"), EventEffect::Ignore);
        assert_eq!(p.window.as_ref().unwrap().title, "notes");
        assert_eq!(p.feed("activewindow>>editor,buffer"), EventEffect::Ignore);
        assert_eq!(p.window, Some(Window::new("editor", "buffer", "1")));

        assert_eq!(p.feed("screencast>>0"), EventEffect::Sharing(false));
        assert!(!p.is_sharing());
        assert_eq!(p.window, Some(Window::new("editor", "buffer", "1")));
        // Already stopped: `>>0,...` is not another transition.
        assert_eq!(p.feed("screencast>>0,1"), EventEffect::Ignore);
        assert_eq!(p.feed("activewindow>>kitty,shell"), EventEffect::Window);
    }

    #[test]
    fn reconnect_keeps_capture_paused_until_share_stops() {
        use std::cell::RefCell;
        use std::os::unix::net::UnixListener;

        let dir = std::env::temp_dir().join(format!("oc-hypr-reconnect-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let events = UnixListener::bind(dir.join(".socket2.sock")).unwrap();
        let requests = UnixListener::bind(dir.join(".socket.sock")).unwrap();
        let server = std::thread::spawn(move || {
            let snapshot = |title| {
                let (mut request, _) = requests.accept().unwrap();
                let mut command = [0; b"j/activewindow".len()];
                request.read_exact(&mut command).unwrap();
                assert_eq!(&command, b"j/activewindow");
                write!(request, r#"{{"class":"editor","title":"{title}"}}"#).unwrap();
            };
            let (mut first, _) = events.accept().unwrap();
            snapshot("initial");
            first.write_all(b"screencast>>1,0\n").unwrap();
            drop(first);
            let (mut second, _) = events.accept().unwrap();
            snapshot("during share");
            second
                .write_all(b"activewindow>>editor,also during share\nscreencast>>0,0\n")
                .unwrap();
            drop(second);
            snapshot("resumed");
        });
        let seen = RefCell::new(Vec::new());
        let emit = |window: Option<Window>| seen.borrow_mut().push(window.map(|w| w.title));
        let stop = AtomicBool::new(false);
        let mut parser = EventParser::default();
        for _ in 0..2 {
            let err = stream(&dir, &mut parser, &emit, &stop).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::UnexpectedEof);
        }
        std::fs::remove_dir_all(dir).unwrap();
        assert_eq!(
            seen.into_inner(),
            [Some("initial".into()), None, Some("resumed".into())]
        );
        server.join().unwrap();
    }
}
