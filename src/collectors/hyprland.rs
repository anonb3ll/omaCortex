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
    /// Hyprland reported an active screencast. Window capture stays paused.
    pub sharing: bool,
}

impl EventParser {
    pub fn seed(&mut self, window: Option<Window>) {
        if let Some(w) = &window {
            self.workspace = w.workspace.clone();
        }
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
        if changed && !self.sharing {
            EventEffect::Window
        } else {
            EventEffect::Ignore
        }
    }

    /// `screencast>>1,...` starts a share. `screencast>>0` (with or without
    /// a trailing owner) stops it. Anything else is ignored.
    fn screencast(&mut self, data: &str) -> EventEffect {
        let state = data.split(',').next().unwrap_or(data).trim();
        let sharing = match state {
            "1" => true,
            "0" => false,
            _ => return EventEffect::Ignore,
        };
        if sharing == self.sharing {
            return EventEffect::Ignore;
        }
        self.sharing = sharing;
        EventEffect::Sharing(sharing)
    }
}

pub fn run(on_window: impl Fn(Option<Window>), stop: &AtomicBool) {
    let mut backoff = 1.0_f64;
    while !stop.load(Ordering::Relaxed) {
        match socket_dir() {
            None => eprintln!("omacortex: hyprland: no running instance found; retrying"),
            Some(dir) => match stream(&dir, &on_window, stop) {
                Ok(()) => backoff = 1.0,
                Err(e) => eprintln!("omacortex: hyprland: event socket: {e}"),
            },
        }
        wait(stop, backoff);
        backoff = (backoff * 2.0).min(30.0);
    }
}

fn stream(
    dir: &Path,
    on_window: &impl Fn(Option<Window>),
    stop: &AtomicBool,
) -> std::io::Result<()> {
    let mut parser = EventParser::default();
    parser.seed(active_window(dir));
    on_window(parser.window.clone());
    let sock = UnixStream::connect(dir.join(".socket2.sock"))?;
    sock.set_read_timeout(Some(Duration::from_secs(1)))?;
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
        assert!(!p.sharing);

        assert_eq!(p.feed("screencast>>1,0"), EventEffect::Sharing(true));
        assert!(p.sharing);
        // A second start, and a non-0/1 state, do not flap the pause.
        assert_eq!(p.feed("screencast>>1,1"), EventEffect::Ignore);
        assert_eq!(p.feed("screencast>>2,0"), EventEffect::Ignore);
        assert!(p.sharing);

        // Focus still updates, but nothing is emitted while sharing.
        assert_eq!(p.feed("activewindowv2>>0xabc"), EventEffect::Ignore);
        assert_eq!(p.feed("windowtitlev2>>0xabc,notes"), EventEffect::Ignore);
        assert_eq!(p.window.as_ref().unwrap().title, "notes");
        assert_eq!(p.feed("activewindow>>editor,buffer"), EventEffect::Ignore);
        assert_eq!(p.window, Some(Window::new("editor", "buffer", "1")));

        assert_eq!(p.feed("screencast>>0"), EventEffect::Sharing(false));
        assert!(!p.sharing);
        assert_eq!(p.window, Some(Window::new("editor", "buffer", "1")));
        // Already stopped: `>>0,...` is not another transition.
        assert_eq!(p.feed("screencast>>0,1"), EventEffect::Ignore);
        assert_eq!(p.feed("activewindow>>kitty,shell"), EventEffect::Window);
    }
}
