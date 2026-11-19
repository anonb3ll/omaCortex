//! Files you open, from the freedesktop recently-used list.
//!
//! GTK, Qt and many other apps already append every opened file to
//! `~/.local/share/recently-used.xbel`. omaCortex only reads that list; it
//! never watches the filesystem.

use super::wait;
use chrono::DateTime;
use quick_xml::Reader;
use quick_xml::XmlVersion;
use quick_xml::events::{BytesStart, Event};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

const MAX_BYTES: u64 = 8 * 1024 * 1024;

pub fn xbel_path() -> PathBuf {
    match std::env::var_os("XDG_DATA_HOME") {
        Some(v) if Path::new(&v).is_absolute() => PathBuf::from(v),
        _ => crate::config::home().join(".local/share"),
    }
    .join("recently-used.xbel")
}

fn ts(value: Option<String>) -> Option<f64> {
    DateTime::parse_from_rfc3339(&value?)
        .ok()
        .map(|d| d.timestamp() as f64 + f64::from(d.timestamp_subsec_millis()) / 1000.0)
}

fn attr(e: &BytesStart, name: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == name)
        .and_then(|a| {
            a.normalized_value(XmlVersion::Implicit1_0)
                .ok()
                .map(|v| v.into_owned())
        })
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// (timestamp, path, app) for each local file bookmark, oldest first.
pub fn parse_xbel(text: &str) -> Vec<(f64, String, String)> {
    let mut reader = Reader::from_str(text);
    let mut out = Vec::new();
    // Current bookmark: path, fallback time, best application time and name.
    let mut cur: Option<(String, Option<f64>, Option<f64>, String)> = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                if e.name().as_ref() == "bookmark" {
                    if let Some(c) = cur.take() {
                        push(&mut out, c);
                    }
                    let href = attr(&e, "href").unwrap_or_default();
                    if let Some(path) = href.strip_prefix("file://") {
                        let fallback = ts(attr(&e, "visited"))
                            .or(ts(attr(&e, "modified")))
                            .or(ts(attr(&e, "added")));
                        cur = Some((percent_decode(path), fallback, None, String::new()));
                    }
                } else if e.name().as_ref() == "bookmark:application"
                    && let Some(c) = cur.as_mut()
                {
                    let t = ts(attr(&e, "modified"));
                    if t.is_some() && (c.2.is_none() || t > c.2) {
                        c.2 = t;
                        c.3 = attr(&e, "name").unwrap_or_default();
                    }
                }
            }
            Ok(Event::End(e)) if e.name().as_ref() == "bookmark" => {
                if let Some(c) = cur.take() {
                    push(&mut out, c);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    if let Some(c) = cur.take() {
        push(&mut out, c);
    }
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out
}

fn push(
    out: &mut Vec<(f64, String, String)>,
    (path, fallback, app_ts, app): (String, Option<f64>, Option<f64>, String),
) {
    if let Some(t) = app_ts.or(fallback) {
        out.push((t, path, if app_ts.is_some() { app } else { String::new() }));
    }
}

type OnFile<'a> = Box<dyn Fn(&str, &str, f64) + 'a>;

pub struct FilesCollector<'a> {
    pub path: PathBuf,
    pub watermark: Box<dyn Fn() -> f64 + 'a>,
    pub set_watermark: Box<dyn Fn(f64) + 'a>,
    pub on_file: OnFile<'a>,
    mtime: Option<SystemTime>,
}

impl<'a> FilesCollector<'a> {
    pub fn new(
        path: PathBuf,
        watermark: impl Fn() -> f64 + 'a,
        set_watermark: impl Fn(f64) + 'a,
        on_file: impl Fn(&str, &str, f64) + 'a,
    ) -> Self {
        Self {
            path,
            watermark: Box::new(watermark),
            set_watermark: Box::new(set_watermark),
            on_file: Box::new(on_file),
            mtime: None,
        }
    }

    pub fn poll(&mut self) -> usize {
        let Ok(meta) = std::fs::metadata(&self.path) else {
            return 0;
        };
        let mtime = meta.modified().ok();
        if mtime == self.mtime || meta.len() > MAX_BYTES {
            return 0;
        }
        self.mtime = mtime;
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return 0;
        };
        let since = (self.watermark)();
        let mut newest = since;
        let mut count = 0;
        for (t, path, app) in parse_xbel(&text) {
            if t <= since {
                continue;
            }
            (self.on_file)(&path, &app, t);
            newest = newest.max(t);
            count += 1;
        }
        if newest > since {
            (self.set_watermark)(newest);
        }
        count
    }

    pub fn run(&mut self, stop: &AtomicBool, interval: f64) {
        while !stop.load(Ordering::Relaxed) {
            self.poll();
            wait(stop, interval.max(5.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    const XBEL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
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
</xbel>"#;

    #[test]
    fn xbel_parsing() {
        let rows = parse_xbel(XBEL);
        let got: Vec<_> = rows
            .iter()
            .map(|(_, p, a)| (p.as_str(), a.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("/home/u/old.txt", ""),
                ("/home/u/docs/plan v2.md", "Text Editor")
            ]
        );
        assert!(parse_xbel("<not xml").is_empty());
    }

    #[test]
    fn collector_uses_watermark() {
        let dir = std::env::temp_dir().join(format!("oc-xbel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("recently-used.xbel");
        std::fs::write(&path, XBEL).unwrap();
        let mark = Cell::new(parse_xbel(XBEL)[0].0);
        let seen = RefCell::new(Vec::new());
        let mut c = FilesCollector::new(
            path,
            || mark.get(),
            |v| mark.set(v),
            |p, _, _| seen.borrow_mut().push(p.to_string()),
        );
        assert_eq!(c.poll(), 1);
        assert_eq!(*seen.borrow(), vec!["/home/u/docs/plan v2.md".to_string()]);
        assert_eq!(c.poll(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }
}
