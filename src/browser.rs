//! Browser pages: a native-messaging host and its installer.
//!
//! A window title says "Some talk - YouTube - Chromium" but not which page it
//! was, who made it, or how long it was really in front of you. A browser
//! extension knows, and hands that to this host over Chrome's native
//! messaging: the browser starts `omacortex` with the extension's origin as
//! its first argument and exchanges JSON messages on stdin and stdout, each
//! prefixed with its length as a 32-bit integer in native byte order.
//!
//! The host never opens a network port, and the browser only starts it for
//! the extension ids listed in the manifest `omacortex browser install`
//! writes. What it accepts is metadata: address, title, site, author, kind
//! and time in the foreground. Every message goes through the same consent,
//! pause and privacy rules as the other sensors, and anything else is refused.
//!
//! Protocol version 1, one request, one reply:
//!
//! - `{"v":1,"type":"status"}` replies
//!   `{"ok":true,"consent":…,"paused":…,"recording":…,"capture":…,"min_seconds":…,"version":"…"}`
//! - `{"v":1,"type":"page","url":…,"title":…,"site":…,"author":…,
//!   "content":"video|article|page","seconds":…,"at":…,"source":"…"}` replies
//!   `{"ok":true,"stored":true|false,"reason":…,"recording":…,"version":"…"}`.
//!   `seconds` is foreground time so far and `at` the start of the visit in
//!   epoch seconds; sending the same `url` and `at` again lengthens that visit.
//!   A temporary storage failure returns `retryable: true`.
//! - Anything else replies `{"ok":false,"error":"…"}`.

use crate::api::Cortex;
use crate::config;
use crate::recorder::{Page, PageOutcome, Recorder};
use serde_json::{Value, json};
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub const HOST_NAME: &str = "io.github.anonb3ll.omacortex";
/// The id of the extension in `extension/`, fixed by the key in its manifest.
pub const COMPANION_ID: &str = "kifhcmopbipadknlobfmidehjfgamjdl";
pub const PROTOCOL: u64 = 1;
/// Chrome caps messages to a host at 64 MiB; nothing here needs more than a few KiB.
const MAX_MESSAGE: usize = 64 * 1024;

/// Read one framed message. `Ok(None)` is a clean end of input.
pub fn read_message(input: &mut impl Read) -> io::Result<Option<Value>> {
    let mut len = [0u8; 4];
    match input.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_ne_bytes(len) as usize;
    if len > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    let mut body = vec![0u8; len];
    input.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn write_message(output: &mut impl Write, message: &Value) -> io::Result<()> {
    let body = message.to_string();
    output.write_all(&(body.len() as u32).to_ne_bytes())?;
    output.write_all(body.as_bytes())?;
    output.flush()
}

fn text(msg: &Value, key: &str) -> String {
    msg[key].as_str().unwrap_or("").to_string()
}

pub fn handle(cortex: &Cortex, recorder: &Recorder, msg: &Value) -> Value {
    let version = env!("CARGO_PKG_VERSION");
    if !msg.is_object() {
        return json!({"ok": false, "error": "message must be an object"});
    }
    if msg.get("v").is_some_and(|v| v.as_u64() != Some(PROTOCOL)) {
        return json!({"ok": false, "error": format!("unsupported protocol version; this host speaks {PROTOCOL}")});
    }
    let capture = &cortex.cfg.settings.capture;
    let state = cortex.state.read();
    let recording = state.recording() && capture.browser;
    match msg["type"].as_str().unwrap_or("") {
        "status" => json!({
            "ok": true, "consent": state.consent, "paused": state.paused, "recording": recording,
            "capture": capture.browser, "min_seconds": capture.min_page_seconds, "version": version,
        }),
        "page" => {
            let outcome = if capture.browser {
                recorder.page(
                    &Page {
                        url: text(msg, "url"),
                        title: text(msg, "title"),
                        site: text(msg, "site"),
                        author: text(msg, "author"),
                        content: text(msg, "content"),
                        seconds: msg["seconds"].as_f64().unwrap_or(0.0),
                        at: msg["at"].as_f64(),
                        source: text(msg, "source"),
                    },
                    capture.min_page_seconds as f64,
                )
            } else {
                PageOutcome::Skipped("browser capture is off")
            };
            match outcome {
                PageOutcome::Stored(_) => {
                    json!({"ok": true, "stored": true, "recording": recording, "version": version})
                }
                PageOutcome::Skipped(reason) => {
                    json!({"ok": true, "stored": false, "reason": reason, "retryable": reason == "store error",
                        "recording": recording, "recording_since": state.recording_since, "version": version})
                }
            }
        }
        other => json!({"ok": false, "error": format!("unknown message type: {other:?}")}),
    }
}

/// Answer messages until the browser closes the pipe.
pub fn serve(cortex: &Cortex, mut input: impl Read, mut output: impl Write) -> io::Result<()> {
    let recorder = Recorder::new(
        cortex.store.clone(),
        cortex.privacy.clone(),
        cortex.state.clone(),
        0.0,
        cortex.clock.clone(),
    );
    loop {
        let reply = match read_message(&mut input) {
            Ok(Some(msg)) => handle(cortex, &recorder, &msg),
            Ok(None) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                // The stream cannot be resynchronised after a bad frame.
                write_message(&mut output, &json!({"ok": false, "error": e.to_string()}))?;
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        write_message(&mut output, &reply)?;
    }
}

// -- installer ---------------------------------------------------------------

/// Chromium-family browsers and where each looks for native-messaging hosts,
/// relative to the user's config directory.
const BROWSERS: [(&str, &str); 5] = [
    ("chromium", "chromium"),
    ("chrome", "google-chrome"),
    ("brave", "BraveSoftware/Brave-Browser"),
    ("vivaldi", "vivaldi"),
    ("edge", "microsoft-edge"),
];

pub fn config_base() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(v) if Path::new(&v).is_absolute() => PathBuf::from(v),
        _ => config::home().join(".config"),
    }
}

fn manifest_path(base: &Path, profile_dir: &str) -> PathBuf {
    base.join(profile_dir)
        .join("NativeMessagingHosts")
        .join(format!("{HOST_NAME}.json"))
}

/// Extension ids are 32 letters from a to p.
pub fn valid_extension_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| (b'a'..=b'p').contains(&b))
}

fn origin(id: &str) -> String {
    format!("chrome-extension://{id}/")
}

/// The program the browser should start: the stable `~/.local/bin/omacortex`
/// link when install.sh made one, otherwise this executable.
pub fn host_program() -> PathBuf {
    let linked = config::home().join(".local/bin/omacortex");
    if linked.exists() {
        return linked;
    }
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("omacortex"))
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct HostStatus {
    pub browser: String,
    /// The browser has a config directory on this machine.
    pub present: bool,
    pub installed: bool,
    pub manifest: String,
    pub program: String,
    pub program_ok: bool,
    pub extensions: Vec<String>,
}

pub fn status(base: &Path) -> Vec<HostStatus> {
    BROWSERS
        .iter()
        .map(|(name, dir)| {
            let path = manifest_path(base, dir);
            let manifest: Option<Value> = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok());
            let program = manifest
                .as_ref()
                .and_then(|m| m["path"].as_str())
                .unwrap_or("")
                .to_string();
            let executable = std::fs::metadata(&program)
                .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false);
            HostStatus {
                browser: (*name).to_string(),
                present: base.join(dir).is_dir(),
                installed: manifest.is_some(),
                manifest: path.display().to_string(),
                program_ok: executable,
                program,
                extensions: manifest
                    .as_ref()
                    .and_then(|m| m["allowed_origins"].as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(|o| {
                                o.trim_start_matches("chrome-extension://")
                                    .trim_end_matches('/')
                                    .to_string()
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            }
        })
        .collect()
}

/// Write the host manifest for each named browser, or for every browser that
/// has a config directory when `only` is empty. Extension ids already allowed
/// in an existing manifest are kept. Returns the files written.
pub fn install(
    base: &Path,
    program: &Path,
    extension_ids: &[String],
    only: &[String],
) -> Result<Vec<PathBuf>, String> {
    if let Some(bad) = extension_ids.iter().find(|id| !valid_extension_id(id)) {
        return Err(format!(
            "not an extension id: {bad:?} (32 letters a-p, shown on chrome://extensions)"
        ));
    }
    if let Some(unknown) = only
        .iter()
        .find(|b| !BROWSERS.iter().any(|(name, _)| name == b))
    {
        let names: Vec<&str> = BROWSERS.iter().map(|(n, _)| *n).collect();
        return Err(format!(
            "unknown browser {unknown:?} (one of {})",
            names.join(", ")
        ));
    }
    let mut written = Vec::new();
    for current in status(base) {
        let wanted = if only.is_empty() {
            current.present
        } else {
            only.contains(&current.browser)
        };
        if !wanted {
            continue;
        }
        let mut ids = vec![COMPANION_ID.to_string()];
        for id in current.extensions.iter().chain(extension_ids) {
            if valid_extension_id(id) && !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        let manifest = json!({
            "name": HOST_NAME,
            "description": "omaCortex: a private, local memory of your day",
            "path": program,
            "type": "stdio",
            "allowed_origins": ids.iter().map(|id| origin(id)).collect::<Vec<_>>(),
        });
        let path = PathBuf::from(&current.manifest);
        let dir = path.parent().ok_or("bad manifest path")?;
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let body = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
        std::fs::write(&path, body + "\n").map_err(|e| format!("{}: {e}", path.display()))?;
        written.push(path);
    }
    Ok(written)
}

pub fn uninstall(base: &Path) -> Vec<PathBuf> {
    status(base)
        .into_iter()
        .filter(|s| s.installed)
        .map(|s| PathBuf::from(s.manifest))
        .filter(|p| std::fs::remove_file(p).is_ok())
        .collect()
}

/// Where the companion extension lives, for "Load unpacked".
pub fn extension_dir() -> Option<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(root) = std::env::var_os("OMACORTEX_ROOT") {
        roots.push(PathBuf::from(root));
    }
    if let Ok(exe) = std::env::current_exe().and_then(std::fs::canonicalize) {
        // target/release/omacortex and libexec/omacortex both sit under the plugin root.
        roots.extend(exe.ancestors().skip(1).take(3).map(Path::to_path_buf));
    }
    roots
        .into_iter()
        .map(|r| r.join("extension"))
        .find(|d| d.join("manifest.json").is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Settings};
    use crate::recorder::tests::temp_paths;
    use crate::store::Query;
    use crate::store::tests::TestClock;

    fn cortex(settings: Settings, consent: bool) -> (Cortex, TestClock) {
        let c = TestClock::new();
        let cortex = Cortex::open(
            Config::with_settings(temp_paths("browser"), settings),
            c.clock(),
        )
        .unwrap();
        if consent {
            cortex.state.grant().unwrap();
        }
        (cortex, c)
    }

    fn framed(messages: &[Value]) -> Vec<u8> {
        let mut out = Vec::new();
        for m in messages {
            write_message(&mut out, m).unwrap();
        }
        out
    }

    fn replies(mut bytes: &[u8]) -> Vec<Value> {
        let mut out = Vec::new();
        while let Some(v) = read_message(&mut bytes).unwrap() {
            out.push(v);
        }
        out
    }

    fn page(seconds: f64, at: f64) -> Value {
        json!({"v": 1, "type": "page", "source": "companion", "url": "https://writer.substack.com/p/small-models?utm_source=x",
               "title": "Why small models win", "site": "The Writer", "author": "A. Writer", "content": "article",
               "seconds": seconds, "at": at})
    }

    fn report(cortex: &Cortex, visit: Value) -> Value {
        let mut out = Vec::new();
        serve(cortex, framed(&[visit]).as_slice(), &mut out).unwrap();
        replies(&out).pop().unwrap()
    }

    #[test]
    fn page_visits_cannot_cross_consent_or_resume() {
        let (cortex, clock) = cortex(Settings::default(), false);
        let before_consent = clock.now();
        clock.tick(60.0);
        cortex.state.grant().unwrap();
        assert_eq!(report(&cortex, page(60.0, before_consent))["stored"], false);

        let before_pause = clock.now();
        clock.tick(60.0);
        cortex.state.pause(None).unwrap();
        let while_paused = clock.now();
        clock.tick(60.0);
        assert_eq!(
            report(&cortex, page(60.0, while_paused))["reason"],
            "paused"
        );
        cortex.state.resume().unwrap();
        assert_eq!(
            report(&cortex, page(60.0, while_paused))["reason"],
            "before recording resumed"
        );
        assert_eq!(report(&cortex, page(180.0, before_pause))["stored"], false);

        cortex.state.pause(Some(60.0)).unwrap();
        let timed_pause = clock.now();
        clock.tick(61.0);
        let after_resume = clock.now();
        clock.tick(60.0);
        assert_eq!(report(&cortex, page(120.0, timed_pause))["stored"], false);
        assert_eq!(report(&cortex, page(60.0, after_resume))["stored"], true);
        assert_eq!(cortex.store.export().unwrap().len(), 1);
    }

    #[test]
    fn a_late_page_report_cannot_recreate_a_forgotten_visit() {
        let (cortex, clock) = cortex(Settings::default(), true);
        let started = clock.now();
        clock.tick(60.0);
        assert_eq!(report(&cortex, page(60.0, started))["stored"], true);
        assert_eq!(
            cortex
                .store
                .forget(None, None, Some("writer.substack.com"), false),
            Ok(1)
        );
        let cortex = Cortex::open(cortex.cfg.clone(), clock.clock()).unwrap();
        clock.tick(15.0);
        let stale = report(&cortex, page(75.0, started));
        assert_eq!(stale["reason"], "visit forgotten");
        assert_eq!(stale["retryable"], false);
        assert!(cortex.store.export().unwrap().is_empty());
        let fresh = clock.now();
        clock.tick(60.0);
        assert_eq!(report(&cortex, page(60.0, fresh))["stored"], true);
    }

    #[test]
    fn a_forget_ends_a_surviving_visit_and_the_next_one_records() {
        let (cortex, clock) = cortex(Settings::default(), true);
        let started = clock.now();
        clock.tick(60.0);
        assert_eq!(report(&cortex, page(60.0, started))["stored"], true);
        clock.tick(7200.0);
        // Nothing in the last 15 minutes was this page's foreground time, but
        // it was reported then, so a 15-minute forget removes it.
        let window = clock.now() - 900.0;
        report(&cortex, page(90.0, started));
        assert_eq!(cortex.store.forget(Some(window), None, None, false), Ok(1));
        let stale = report(&cortex, page(120.0, started));
        assert_eq!(
            (&stale["reason"], &stale["retryable"]),
            (&json!("visit forgotten"), &json!(false))
        );
        assert!(cortex.store.export().unwrap().is_empty());
        // The companion starts over once the reply arrives, after the forget.
        clock.tick(1.0);
        let fresh = clock.now();
        clock.tick(60.0);
        assert_eq!(report(&cortex, page(60.0, fresh))["stored"], true);
    }

    #[test]
    fn a_page_is_stored_once_and_lengthened() {
        let (cortex, c) = cortex(Settings::default(), true);
        let t0 = c.now();
        c.tick(900.0);
        let input = framed(&[
            json!({"v": 1, "type": "status"}),
            page(45.0, t0),
            page(600.0, t0),
            json!({"v": 1, "type": "page", "url": "https://accounts.google.com/", "title": "Account", "seconds": 99}),
            json!({"v": 2, "type": "status"}),
            json!({"type": "delete-everything"}),
            json!([1, 2]),
        ]);
        let mut out = Vec::new();
        serve(&cortex, input.as_slice(), &mut out).unwrap();
        let r = replies(&out);
        assert_eq!(r.len(), 7);
        assert_eq!(
            (&r[0]["recording"], &r[0]["min_seconds"]),
            (&json!(true), &json!(10))
        );
        assert_eq!(
            (&r[1]["stored"], &r[2]["stored"]),
            (&json!(true), &json!(true))
        );
        assert_eq!(
            (&r[3]["stored"], &r[3]["reason"]),
            (&json!(false), &json!("excluded"))
        );
        assert!(r[4]["error"].as_str().unwrap().contains("protocol version"));
        assert_eq!(r[5]["ok"], false);
        assert_eq!(r[6]["ok"], false);

        let kinds = ["page".to_string()];
        let rows = cortex
            .store
            .timeline(&Query {
                start: Some(0.0),
                kinds: &kinds,
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].app.as_str(), rows[0].seconds),
            ("writer.substack.com", 600.0)
        );
        assert!(
            rows[0]
                .detail
                .ends_with("https://writer.substack.com/p/small-models")
        );
    }

    #[test]
    fn nothing_is_stored_without_consent_or_with_capture_off() {
        let (cortex, c) = cortex(Settings::default(), false);
        let mut out = Vec::new();
        serve(&cortex, framed(&[page(60.0, c.now())]).as_slice(), &mut out).unwrap();
        assert_eq!(replies(&out)[0]["reason"], "paused");

        let mut settings = Settings::default();
        settings.capture.browser = false;
        let (cortex, c) = super::tests::cortex(settings, true);
        let mut out = Vec::new();
        serve(
            &cortex,
            framed(&[page(60.0, c.now()), json!({"type": "status"})]).as_slice(),
            &mut out,
        )
        .unwrap();
        let r = replies(&out);
        assert_eq!(r[0]["reason"], "browser capture is off");
        assert_eq!(r[1]["recording"], false);
        assert!(cortex.store.export().unwrap().is_empty());
    }

    #[test]
    fn oversized_and_truncated_frames_end_the_session() {
        let (cortex, _) = cortex(Settings::default(), true);
        let mut out = Vec::new();
        let mut huge = (10_000_000u32).to_ne_bytes().to_vec();
        huge.extend_from_slice(b"{}");
        serve(&cortex, huge.as_slice(), &mut out).unwrap();
        assert_eq!(replies(&out)[0]["ok"], false);
        // Half a length prefix is just the end of input.
        let mut out = Vec::new();
        serve(&cortex, [1u8, 0].as_slice(), &mut out).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn install_writes_manifests_and_keeps_allowed_extensions() {
        let base = temp_paths("hosts").config_dir;
        std::fs::create_dir_all(base.join("chromium")).unwrap();
        let program = base.join("omacortex");
        std::fs::write(&program, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let other = "abcdefghijklmnopabcdefghijklmnop".to_string();

        // Only browsers that exist on this machine get a manifest by default.
        let written = install(&base, &program, std::slice::from_ref(&other), &[]).unwrap();
        assert_eq!(written.len(), 1);
        let manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(&written[0]).unwrap()).unwrap();
        assert_eq!(manifest["name"], HOST_NAME);
        assert_eq!(manifest["type"], "stdio");
        assert_eq!(manifest["path"], json!(program));
        // Installing again without the extra id does not revoke it.
        install(&base, &program, &[], &[]).unwrap();
        let s = &status(&base)[0];
        assert!(s.installed && s.program_ok);
        assert_eq!(s.extensions, vec![COMPANION_ID.to_string(), other]);

        assert!(install(&base, &program, &["nope".into()], &[]).is_err());
        assert!(install(&base, &program, &[], &["netscape".into()]).is_err());
        assert_eq!(
            install(&base, &program, &[], &["brave".into()])
                .unwrap()
                .len(),
            1
        );
        assert_eq!(uninstall(&base).len(), 2);
        assert!(status(&base).iter().all(|s| !s.installed));
    }
}
