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
