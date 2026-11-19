//! The capture daemon.
//!
//! One process per user (guarded by a lock file). It runs the enabled sensors
//! in threads, re-reads consent and pause state every two seconds, writes a
//! heartbeat so a crash never leaves a span open forever, enforces the
//! retention window, and listens on a private Unix socket for "away"/"back"
//! reports from the shell plugin.

use crate::collectors::{self, files::FilesCollector, hyprland, media};
use crate::config::Config;
use crate::privacy::Privacy;
use crate::recorder::Recorder;
use crate::state::State;
use crate::store::Store;
use crate::timeutil::{self, Clock};
use serde_json::{Value, json};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const TICK: f64 = 2.0;
const HEARTBEAT_EVERY: f64 = 30.0;
const PURGE_EVERY: f64 = 3600.0;

static SIGNALLED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    SIGNALLED.store(true, Ordering::SeqCst);
}

pub enum LockError {
    AlreadyRunning,
    Io(std::io::Error),
}

pub fn acquire_lock(cfg: &Config) -> Result<File, LockError> {
    let f = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(cfg.paths.lock_file())
        .map_err(LockError::Io)?;
    match f.try_lock() {
        Ok(()) => Ok(f),
        Err(std::fs::TryLockError::WouldBlock) => Err(LockError::AlreadyRunning),
        Err(std::fs::TryLockError::Error(e)) => Err(LockError::Io(e)),
    }
}

pub struct Daemon {
    pub cfg: Config,
    pub store: Arc<Store>,
    pub recorder: Arc<Recorder>,
    pub stop: Arc<AtomicBool>,
    clock: Clock,
}

impl Daemon {
    pub fn new(
        cfg: Config,
        clock: Clock,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        cfg.paths.ensure()?;
        let store = Arc::new(Store::open(&cfg.paths.db(), clock.clone())?);
        let state = State::new(cfg.paths.clone(), clock.clone());
        let recorder = Arc::new(Recorder::new(
            store.clone(),
            Privacy::from_settings(&cfg.settings.privacy),
            state,
            cfg.settings.capture.min_span_seconds as f64,
            clock.clone(),
        ));
        Ok(Self {
            cfg,
            store,
            recorder,
            stop: Arc::new(AtomicBool::new(false)),
            clock,
        })
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    /// Close spans a crash left open, at the last heartbeat.
    pub fn recover(&self) {
        let at = self
            .store
            .get_meta("heartbeat")
            .and_then(|h| h.parse::<f64>().ok())
            .unwrap_or_else(|| self.now());
        if let Ok(n) = self.store.close_all_open(at.min(self.now()))
            && n > 0
        {
            eprintln!("omacortex: closed {n} span(s) left open by a previous run");
        }
        let _ = self.store.set_meta("away", "0");
    }

    pub fn handle(&self, msg: &Value) -> Value {
        match msg["cmd"].as_str().unwrap_or("") {
            "away" => {
                self.recorder.set_idle(true, None);
                let _ = self.store.set_meta("away", "1");
            }
            "back" => {
                self.recorder.set_idle(false, None);
                let _ = self.store.set_meta("away", "0");
            }
            "refresh" => {
                self.recorder.refresh_state(None);
            }
            "ping" => {}
            other => return json!({"ok": false, "error": format!("unknown command: {other:?}")}),
        }
        json!({"ok": true, "recording": self.recorder.recording()})
    }

    pub fn serve_control(self: &Arc<Self>) -> std::io::Result<()> {
        let path = self.cfg.paths.control_socket();
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        while !self.stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((conn, _)) => {
                    let _ = conn.set_nonblocking(false);
                    let _ = conn.set_read_timeout(Some(Duration::from_secs(2)));
                    let mut line = String::new();
                    let reply = match BufReader::new(&conn).take_line(&mut line) {
                        Ok(()) => match serde_json::from_str::<Value>(line.trim()) {
                            Ok(v) => self.handle(&v),
                            Err(e) => json!({"ok": false, "error": e.to_string()}),
                        },
                        Err(e) => json!({"ok": false, "error": e.to_string()}),
                    };
                    let _ = (&conn).write_all(format!("{reply}\n").as_bytes());
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(200))
                }
                Err(_) => std::thread::sleep(Duration::from_millis(500)),
            }
        }
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    fn spawn(self: &Arc<Self>) {
        let cap = &self.cfg.settings.capture;
        if cap.windows {
            let d = self.clone();
            std::thread::Builder::new()
                .name("hyprland".into())
                .spawn(move || hyprland::run(|w| d.recorder.focus(w, None), &d.stop))
                .ok();
        }
        if cap.media {
            let d = self.clone();
            let interval = cap.media_poll_seconds as f64;
            std::thread::Builder::new()
                .name("media".into())
                .spawn(move || media::run(|t| d.recorder.media(t, None), &d.stop, interval))
                .ok();
        }
        if cap.files {
            if self.store.get_meta("files_watermark").is_none() {
                // Do not import the whole history of the recent list on first run.
                let _ = self
                    .store
                    .set_meta("files_watermark", &self.now().to_string());
            }
            let d = self.clone();
            let interval = cap.files_poll_seconds as f64;
            std::thread::Builder::new()
                .name("files".into())
                .spawn(move || {
                    let mut c = FilesCollector::new(
                        collectors::files::xbel_path(),
                        || {
                            d.store
                                .get_meta("files_watermark")
                                .and_then(|v| v.parse().ok())
                                .unwrap_or_else(|| (d.clock)())
                        },
                        |ts| {
                            let _ = d.store.set_meta("files_watermark", &ts.to_string());
                        },
                        |path, app, ts| {
                            d.recorder.file_opened(path, app, Some(ts));
                        },
                    );
                    c.run(&d.stop, interval);
                })
                .ok();
        }
        let d = self.clone();
        std::thread::Builder::new()
            .name("control".into())
            .spawn(move || {
                if let Err(e) = d.serve_control() {
                    eprintln!(
                        "omacortex: control socket unavailable ({e}); away/back reports are ignored"
                    );
                }
            })
            .ok();
    }

    fn purge(&self, now: f64) {
        let days = self.cfg.settings.privacy.retention_days;
        if days == 0 {
            return;
        }
        let cutoff = now - days as f64 * 86400.0;
        let day = timeutil::day_bounds("today", cutoff)
            .map(|d| d.0)
            .unwrap_or_default();
        if let Ok(n) = self.store.purge_older_than(cutoff, &day)
            && n > 0
        {
            eprintln!("omacortex: retention: removed {n} row(s) older than {days} days");
        }
    }

    pub fn run(self: &Arc<Self>) {
        self.recover();
        self.spawn();
        let (mut last_beat, mut last_purge) = (0.0, 0.0);
        while !self.stop.load(Ordering::Relaxed) && !SIGNALLED.load(Ordering::Relaxed) {
            let now = self.now();
            self.recorder.refresh_state(Some(now));
            if now - last_beat >= HEARTBEAT_EVERY {
                let _ = self.store.set_meta("heartbeat", &now.to_string());
                last_beat = now;
            }
            if now - last_purge >= PURGE_EVERY {
                self.purge(now);
                last_purge = now;
            }
            collectors::wait(&SIGNALLED, TICK);
        }
        self.stop.store(true, Ordering::SeqCst);
        self.recorder.shutdown(None);
        let _ = self.store.set_meta("heartbeat", &self.now().to_string());
        std::thread::sleep(Duration::from_millis(300)); // let the control thread remove its socket
    }
}

trait TakeLine {
    fn take_line(self, buf: &mut String) -> std::io::Result<()>;
}

impl<R: std::io::Read> TakeLine for BufReader<R> {
    fn take_line(self, buf: &mut String) -> std::io::Result<()> {
        use std::io::Read;
        self.take(4096).read_line(buf).map(|_| ())
    }
}

pub fn run(cfg: Config, if_not_running: bool) -> i32 {
    let lock = match acquire_lock(&cfg) {
        Ok(f) => f,
        Err(LockError::AlreadyRunning) => {
            if if_not_running {
                return 0;
            }
            eprintln!("omacortex: another omacortex daemon is already running");
            return 1;
        }
        Err(LockError::Io(e)) => {
            eprintln!("omacortex: cannot take the daemon lock: {e}");
            return 1;
        }
    };
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
    }
    let daemon = match Daemon::new(cfg, timeutil::system_clock()) {
        Ok(d) => Arc::new(d),
        Err(e) => {
            eprintln!("omacortex: {e}");
            return 1;
        }
    };
    daemon.run();
    drop(lock);
    0
}

pub fn send_control(cfg: &Config, cmd: &str) -> std::io::Result<Value> {
    let mut s = UnixStream::connect(cfg.paths.control_socket())?;
    s.set_read_timeout(Some(Duration::from_secs(2)))?;
    s.write_all(format!("{}\n", json!({"cmd": cmd})).as_bytes())?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;
    use crate::recorder::Window;
    use crate::recorder::tests::temp_paths;
    use crate::store::Query;
    use crate::store::tests::TestClock;

    #[test]
    fn crash_recovery_and_control_socket() {
        let c = TestClock::new();
        let cfg = Config::with_settings(temp_paths("dmn"), Settings::default());
        let d = Arc::new(Daemon::new(cfg.clone(), c.clock()).unwrap());
        State::new(cfg.paths.clone(), c.clock()).grant().unwrap();
        d.recorder.refresh_state(None);
        d.recorder
            .focus(Some(Window::new("kitty", "shell", "")), None);
        d.store
            .set_meta("heartbeat", &(c.now() + 30.0).to_string())
            .unwrap();
        c.tick(500.0);
        // A new daemon closes the span left open at the last heartbeat.
        let d2 = Daemon::new(cfg.clone(), c.clock()).unwrap();
        d2.recover();
        let k = ["focus".to_string()];
        let span = &d2
            .store
            .timeline(&Query {
                start: Some(0.0),
                kinds: &k,
                limit: 10,
                ..Default::default()
            })
            .unwrap()[0];
        assert!(!span.ongoing);
        assert_eq!(span.seconds, 30.0);

        let d3 = Arc::new(Daemon::new(cfg.clone(), c.clock()).unwrap());
        let srv = d3.clone();
        let t = std::thread::spawn(move || srv.serve_control().unwrap());
        for _ in 0..50 {
            if cfg.paths.control_socket().exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(send_control(&cfg, "ping").unwrap()["ok"], true);
        assert_eq!(send_control(&cfg, "away").unwrap()["recording"], false);
        assert_eq!(d3.store.get_meta("away").as_deref(), Some("1"));
        assert_eq!(send_control(&cfg, "bogus").unwrap()["ok"], false);
        let mode = std::fs::metadata(cfg.paths.control_socket())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        d3.stop.store(true, Ordering::SeqCst);
        t.join().unwrap();
    }

    #[test]
    fn single_instance_lock() {
        let cfg = Config::with_settings(temp_paths("lock"), Settings::default());
        let _held = acquire_lock(&cfg).ok().unwrap();
        assert!(matches!(acquire_lock(&cfg), Err(LockError::AlreadyRunning)));
    }
}
