//! Consent and pause state, kept as small files in the state directory.
//!
//! Recording happens only when consent has been given *and* capture is not
//! paused. Both are plain files, so the CLI, the shell plugin and the daemon
//! agree without talking to each other.

use crate::config::Paths;
use crate::timeutil::Clock;
use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

fn read(path: &Path) -> Value {
    fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(bytes)?;
    }
    fs::rename(tmp, path)
}

fn write(path: &Path, value: Value) -> std::io::Result<()> {
    write_private(path, value.to_string().as_bytes())
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecordingState {
    pub consent: bool,
    pub paused: bool,
    pub paused_until: Option<f64>,
    pub recording_since: f64,
}

impl RecordingState {
    pub fn recording(&self) -> bool {
        self.consent && !self.paused
    }
}

#[derive(Clone)]
pub struct State {
    paths: Paths,
    clock: Clock,
}

impl State {
    pub fn new(paths: Paths, clock: Clock) -> Self {
        Self { paths, clock }
    }

    pub fn read(&self) -> RecordingState {
        let consent = read(&self.paths.consent_file());
        let pause = read(&self.paths.pause_file());
        let mut paused = pause["paused"] == json!(true);
        let until = pause["until"].as_f64();
        let mut recording_since = consent["at"].as_f64().unwrap_or_default();
        if paused && until.is_some_and(|u| u <= (self.clock)()) {
            paused = false;
            recording_since = recording_since.max(until.unwrap_or_default());
        } else if !paused {
            recording_since = recording_since.max(pause["at"].as_f64().unwrap_or_default());
        }
        RecordingState {
            consent: consent["granted"] == json!(true),
            paused,
            paused_until: if paused { until } else { None },
            recording_since,
        }
    }

    pub fn grant(&self) -> std::io::Result<()> {
        write(
            &self.paths.consent_file(),
            json!({"granted": true, "at": (self.clock)()}),
        )
    }

    pub fn revoke(&self) -> std::io::Result<()> {
        write(
            &self.paths.consent_file(),
            json!({"granted": false, "at": (self.clock)()}),
        )
    }

    pub fn pause(&self, seconds: Option<f64>) -> std::io::Result<()> {
        let now = (self.clock)();
        let until = seconds.filter(|s| *s > 0.0).map(|s| now + s);
        write(
            &self.paths.pause_file(),
            json!({"paused": true, "until": until, "at": now}),
        )
    }

    pub fn resume(&self) -> std::io::Result<()> {
        write(
            &self.paths.pause_file(),
            json!({"paused": false, "at": (self.clock)()}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new() -> Self {
            static N: AtomicU64 = AtomicU64::new(0);
            let n = N.fetch_add(1, Ordering::SeqCst);
            let dir =
                std::env::temp_dir().join(format!("omacortex-state-{}-{n}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn paths(&self) -> Paths {
            Paths {
                config_dir: self.0.join("config"),
                data_dir: self.0.join("data"),
                state_dir: self.0.join("state"),
            }
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn clock_at(ticks: &Arc<AtomicU64>) -> Clock {
        let ticks = Arc::clone(ticks);
        Arc::new(move || ticks.load(Ordering::SeqCst) as f64)
    }

    fn state_at(dir: &Scratch, now: u64) -> (State, Arc<AtomicU64>) {
        let ticks = Arc::new(AtomicU64::new(now));
        let state = State::new(dir.paths(), clock_at(&ticks));
        (state, ticks)
    }

    #[test]
    fn recording_requires_consent_and_no_pause() {
        assert!(
            RecordingState {
                consent: true,
                paused: false,
                paused_until: None,
                recording_since: 0.0,
            }
            .recording()
        );
        assert!(
            !RecordingState {
                consent: false,
                paused: false,
                paused_until: None,
                recording_since: 0.0,
            }
            .recording()
        );
        assert!(
            !RecordingState {
                consent: true,
                paused: true,
                paused_until: None,
                recording_since: 0.0,
            }
            .recording()
        );
    }

    #[test]
    fn missing_files_mean_not_recording() {
        let dir = Scratch::new();
        let (state, _) = state_at(&dir, 100);
        let got = state.read();
        assert!(!got.consent);
        assert!(!got.paused);
        assert_eq!(got.paused_until, None);
        assert_eq!(got.recording_since, 0.0);
        assert!(!got.recording());
        assert!(!dir.paths().consent_file().exists());
        assert!(!dir.paths().pause_file().exists());
    }

    #[test]
    fn grant_revoke_and_files_are_private() {
        let dir = Scratch::new();
        let (state, ticks) = state_at(&dir, 100);
        state.grant().unwrap();
        let granted = state.read();
        assert!(granted.consent);
        assert!(granted.recording());
        assert_eq!(granted.recording_since, 100.0);
        let consent = dir.paths().consent_file();
        let mode = fs::metadata(&consent).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(!consent.with_extension("tmp").exists());

        ticks.store(140, Ordering::SeqCst);
        state.revoke().unwrap();
        let revoked = state.read();
        assert!(!revoked.consent);
        assert!(!revoked.recording());
        assert_eq!(revoked.recording_since, 140.0);
    }

    #[test]
    fn pause_and_resume() {
        let dir = Scratch::new();
        let (state, ticks) = state_at(&dir, 100);
        state.grant().unwrap();

        ticks.store(150, Ordering::SeqCst);
        state.pause(Some(60.0)).unwrap();
        let paused = state.read();
        assert!(paused.consent);
        assert!(paused.paused);
        assert!(!paused.recording());
        assert_eq!(paused.paused_until, Some(210.0));
        assert_eq!(paused.recording_since, 100.0);

        ticks.store(180, Ordering::SeqCst);
        assert!(state.read().paused);
        assert_eq!(state.read().paused_until, Some(210.0));

        ticks.store(220, Ordering::SeqCst);
        state.resume().unwrap();
        let resumed = state.read();
        assert!(!resumed.paused);
        assert_eq!(resumed.paused_until, None);
        assert!(resumed.recording());
        assert_eq!(resumed.recording_since, 220.0);
    }

    #[test]
    fn indefinite_and_non_positive_pauses_have_no_deadline() {
        let dir = Scratch::new();
        let (state, ticks) = state_at(&dir, 50);
        state.grant().unwrap();
        state.pause(None).unwrap();
        let open = state.read();
        assert!(open.paused);
        assert_eq!(open.paused_until, None);
        assert_eq!(open.recording_since, 50.0);

        ticks.store(80, Ordering::SeqCst);
        state.pause(Some(0.0)).unwrap();
        assert!(state.read().paused);
        assert_eq!(state.read().paused_until, None);

        state.pause(Some(-5.0)).unwrap();
        assert!(state.read().paused);
        assert_eq!(state.read().paused_until, None);
    }

    #[test]
    fn timed_pause_expires_without_rewriting_the_file() {
        let dir = Scratch::new();
        let (state, ticks) = state_at(&dir, 100);
        state.grant().unwrap();
        ticks.store(150, Ordering::SeqCst);
        state.pause(Some(50.0)).unwrap();
        let on_disk = fs::read_to_string(dir.paths().pause_file()).unwrap();

        ticks.store(199, Ordering::SeqCst);
        let still = state.read();
        assert!(still.paused);
        assert_eq!(still.paused_until, Some(200.0));
        assert_eq!(still.recording_since, 100.0);

        ticks.store(200, Ordering::SeqCst);
        let expired = state.read();
        assert!(!expired.paused);
        assert_eq!(expired.paused_until, None);
        assert!(expired.recording());
        assert_eq!(expired.recording_since, 200.0);
        assert_eq!(
            fs::read_to_string(dir.paths().pause_file()).unwrap(),
            on_disk
        );
    }

    #[test]
    fn unpaused_recording_since_follows_the_later_timestamp() {
        let dir = Scratch::new();
        let (state, ticks) = state_at(&dir, 300);
        state.grant().unwrap();
        ticks.store(220, Ordering::SeqCst);
        state.resume().unwrap();
        let got = state.read();
        assert!(got.recording());
        assert_eq!(got.recording_since, 300.0);
    }

    #[test]
    fn corrupt_state_files_are_ignored() {
        let dir = Scratch::new();
        let paths = dir.paths();
        fs::create_dir_all(&paths.state_dir).unwrap();
        fs::write(paths.consent_file(), "not json").unwrap();
        fs::write(paths.pause_file(), "[1, 2, 3]").unwrap();
        let (state, _) = state_at(&dir, 10);
        let got = state.read();
        assert!(!got.consent);
        assert!(!got.paused);
        assert_eq!(got.recording_since, 0.0);

        fs::write(paths.consent_file(), r#"{"granted":"yes","at":5}"#).unwrap();
        fs::write(paths.pause_file(), r#"{"paused":1,"until":9,"at":4}"#).unwrap();
        let got = state.read();
        assert!(!got.consent);
        assert!(!got.paused);
        assert_eq!(got.recording_since, 5.0);
    }

    #[test]
    fn write_private_replaces_the_file_and_creates_parents() {
        let dir = Scratch::new();
        let path = dir.0.join("nested").join("note.json");
        write_private(&path, b"one").unwrap();
        write_private(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert!(!path.with_extension("tmp").exists());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
