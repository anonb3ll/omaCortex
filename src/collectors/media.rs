//! What is playing, from MPRIS on the session bus.
//!
//! Uses `busctl --user --json=short` (part of systemd, always present on
//! Omarchy) instead of a D-Bus library, and polls: one cheap call per player
//! every few seconds.

use super::wait;
use crate::recorder::Track;
use serde_json::Value;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const IFACE: &str = "org.mpris.MediaPlayer2.Player";

fn busctl(args: &[&str]) -> String {
    Command::new("busctl")
        .args(["--user", "--timeout=2", "--json=short"])
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

pub fn available() -> bool {
    Command::new("busctl")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

pub fn list_players() -> Vec<String> {
    let mut names: Vec<String> = serde_json::from_str::<Value>(&busctl(&["list", "--acquired"]))
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|e| e["name"].as_str())
        .filter(|n| n.starts_with(PREFIX))
        .map(String::from)
        .collect();
    names.sort();
    names.dedup();
    names
}

/// org.mpris.MediaPlayer2.chromium.instance123 -> chromium
pub fn player_label(bus_name: &str) -> String {
    bus_name
        .strip_prefix(PREFIX)
        .map_or(bus_name, |rest| rest.split('.').next().unwrap_or(rest))
        .to_string()
}

/// Parse the two JSON lines of `busctl get-property ... PlaybackStatus Metadata`.
pub fn parse_properties(text: &str) -> (String, Value) {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let (Some(a), Some(b)) = (lines.next(), lines.next()) else {
        return (String::new(), Value::Null);
    };
    let status = serde_json::from_str::<Value>(a)
        .ok()
        .and_then(|v| v["data"].as_str().map(String::from));
    let meta = serde_json::from_str::<Value>(b)
        .ok()
        .map(|v| v["data"].clone());
    match (status, meta) {
        (Some(s), Some(m)) if m.is_object() => (s, m),
        _ => (String::new(), Value::Null),
    }
}

pub fn track_from(bus_name: &str, status: &str, meta: &Value) -> Option<Track> {
    if status != "Playing" {
        return None;
    }
    let field = |k: &str| &meta[k]["data"];
    let title = field("xesam:title").as_str()?.trim();
    if title.is_empty() {
        return None;
    }
    let artist = match field("xesam:artist") {
        Value::Array(a) => a
            .iter()
            .filter_map(Value::as_str)
            .take(3)
            .collect::<Vec<_>>()
            .join(", "),
        Value::String(s) => s.clone(),
        _ => String::new(),
    };
    // Local file paths stay out of media rows; the files sensor owns those.
    let url = field("xesam:url")
        .as_str()
        .filter(|u| !u.starts_with("file:"))
        .unwrap_or_default();
    Some(Track {
        player: player_label(bus_name),
        title: title.to_string(),
        artist,
        url: url.to_string(),
    })
}

pub fn playing() -> Vec<Track> {
    list_players()
        .iter()
        .filter_map(|name| {
            let (status, meta) = parse_properties(&busctl(&[
                "get-property",
                name,
                PATH,
                IFACE,
                "PlaybackStatus",
                "Metadata",
            ]));
            track_from(name, &status, &meta)
        })
        .collect()
}

pub fn run(on_tracks: impl Fn(&[Track]), stop: &AtomicBool, interval: f64) {
    if !available() {
        eprintln!("omacortex: media: busctl not found; media capture disabled");
        return;
    }
    while !stop.load(Ordering::Relaxed) {
        on_tracks(&playing());
        wait(stop, interval.max(1.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mpris_parsing() {
        let status = json!({"type": "s", "data": "Playing"}).to_string();
        let meta = json!({"type": "a{sv}", "data": {
            "xesam:title": {"type": "s", "data": "A talk"},
            "xesam:artist": {"type": "as", "data": ["Speaker"]},
            "xesam:url": {"type": "s", "data": "https://video.example/watch?v=1"}
        }})
        .to_string();
        let (st, md) = parse_properties(&format!("{status}\n{meta}\n"));
        let t = track_from("org.mpris.MediaPlayer2.chromium.instance42", &st, &md).unwrap();
        assert_eq!(
            (t.player.as_str(), t.title.as_str(), t.artist.as_str()),
            ("chromium", "A talk", "Speaker")
        );
        assert!(track_from("org.mpris.MediaPlayer2.mpv", "Paused", &md).is_none());
        let mut local = md.clone();
        local["xesam:url"]["data"] = json!("file:///home/u/film.mkv");
        assert_eq!(
            track_from("org.mpris.MediaPlayer2.mpv", "Playing", &local)
                .unwrap()
                .url,
            ""
        );
        assert_eq!(parse_properties("").0, "");
        assert_eq!(player_label("org.mpris.MediaPlayer2.spotify"), "spotify");
    }
}
