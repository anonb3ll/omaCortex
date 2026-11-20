//! The activity store: one SQLite file with full-text search.
//!
//! Each `activity` row is either a span (focus on a window, a track playing)
//! with `started_at`/`ended_at`, or a point event (a file opened, a note)
//! whose `ended_at` equals `started_at`. A span still in progress has
//! `ended_at` NULL; readers treat it as ending now. The schema is the same one
//! the 0.1 Python recorder wrote, so existing databases keep working.
//!
//! A page visit reported by a browser is a span too. Its `meta` column holds
//! the address, site, author and content type as JSON; the other kinds leave
//! `meta` empty. A page's `ended_at` is its start plus the seconds it was in
//! front, which can be long before you last looked at it, so `meta.seen` also
//! keeps the wall-clock time of its latest report. Forgetting a time window
//! matches a page on whichever is later.

use crate::timeutil::Clock;
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params, params_from_iter};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::Mutex;

pub const KINDS: [&str; 6] = ["focus", "media", "page", "file", "note", "marker"];
/// Kinds that expire with `retention_days`. Everything else is the library
/// (what you watched, read, opened and noted) and follows `library_days`.
const HISTORY_KINDS: &str = "('focus','marker')";
const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS activity (
  id INTEGER PRIMARY KEY,
  kind TEXT NOT NULL,
  source TEXT NOT NULL,
  app TEXT NOT NULL DEFAULT '',
  title TEXT NOT NULL DEFAULT '',
  detail TEXT NOT NULL DEFAULT '',
  started_at REAL NOT NULL,
  ended_at REAL,
  meta TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS activity_started ON activity(started_at);
CREATE INDEX IF NOT EXISTS activity_kind_started ON activity(kind, started_at);
CREATE INDEX IF NOT EXISTS activity_open ON activity(ended_at) WHERE ended_at IS NULL;

CREATE VIRTUAL TABLE IF NOT EXISTS activity_fts USING fts5(
  app, title, detail, content='activity', content_rowid='id', tokenize='unicode61'
);
CREATE TRIGGER IF NOT EXISTS activity_ai AFTER INSERT ON activity BEGIN
  INSERT INTO activity_fts(rowid, app, title, detail) VALUES (new.id, new.app, new.title, new.detail);
END;
CREATE TRIGGER IF NOT EXISTS activity_ad AFTER DELETE ON activity BEGIN
  INSERT INTO activity_fts(activity_fts, rowid, app, title, detail)
  VALUES ('delete', old.id, old.app, old.title, old.detail);
END;
CREATE TRIGGER IF NOT EXISTS activity_au AFTER UPDATE OF app, title, detail ON activity BEGIN
  INSERT INTO activity_fts(activity_fts, rowid, app, title, detail)
  VALUES ('delete', old.id, old.app, old.title, old.detail);
  INSERT INTO activity_fts(rowid, app, title, detail) VALUES (new.id, new.app, new.title, new.detail);
END;

CREATE TABLE IF NOT EXISTS summaries (
  day TEXT NOT NULL,
  scope TEXT NOT NULL DEFAULT 'day',
  model TEXT NOT NULL DEFAULT '',
  text TEXT NOT NULL,
  created_at REAL NOT NULL,
  PRIMARY KEY (day, scope)
);

CREATE TABLE IF NOT EXISTS meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
"#;

const COLUMNS: &str = "a.id, a.kind, a.source, a.app, a.title, a.detail, a.started_at, a.ended_at";

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Row {
    pub id: i64,
    pub kind: String,
    pub source: String,
    pub app: String,
    pub title: String,
    pub detail: String,
    pub started_at: f64,
    pub ended_at: f64,
    pub ongoing: bool,
    pub seconds: f64,
}

/// One distinct thing (a video, an article, a track, a file, a window title)
/// with every time it was seen folded together.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Item {
    pub kind: String,
    pub title: String,
    /// Window class, media player, or web host.
    pub app: String,
    pub site: String,
    /// Artist, channel or author, when known.
    pub creator: String,
    /// Web address or file path, when there is one.
    pub location: String,
    /// "video", "article" or "page" for web pages; empty otherwise.
    pub content: String,
    pub first_seen: f64,
    pub last_seen: f64,
    pub seconds: f64,
    pub times: i64,
    /// Row id of the latest sighting, for `omacortex around --id`.
    pub last_id: i64,
}

/// A page visit as the browser reported it, already cleaned by the privacy guard.
pub struct PageVisit<'a> {
    pub source: &'a str,
    pub host: &'a str,
    pub title: &'a str,
    pub url: &'a str,
    pub site: &'a str,
    pub author: &'a str,
    pub content: &'a str,
    pub at: f64,
    pub seconds: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Usage {
    pub app: String,
    pub seconds: f64,
    pub spans: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub day: String,
    pub model: String,
    pub text: String,
    pub created_at: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    pub rows: BTreeMap<String, i64>,
    pub oldest: Option<f64>,
    pub bytes: u64,
}

#[derive(Default)]
pub struct Query<'a> {
    pub start: Option<f64>,
    pub end: Option<f64>,
    pub kinds: &'a [String],
    pub min_seconds: f64,
    pub limit: usize,
    pub newest_first: bool,
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// Free text to a safe FTS5 query: every word must match, as a prefix.
pub fn fts_query(text: &str) -> String {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.' || c == '-'))
        .filter(|w| w.chars().any(|c| c != '.' && c != '-'))
        .take(12)
        .map(|w| format!("\"{w}\"*"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Free text to substring patterns for LIKE: every word must appear somewhere.
fn like_patterns(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .take(8)
        .map(|w| {
            let escaped: String = w
                .chars()
                .flat_map(|c| match c {
                    '%' | '_' | '\\' => vec!['\\', c],
                    c => vec![c],
                })
                .collect();
            format!("%{escaped}%")
        })
        .collect()
}

/// How a text query was satisfied, most exact first.
pub const MATCH_ALL: &str = "all-words";
pub const MATCH_SUBSTRING: &str = "substring";
pub const MATCH_ANY: &str = "any-word";

/// Split a media or file row's detail into (creator, location).
fn split_detail(kind: &str, detail: &str) -> (String, String) {
    if kind == "file" {
        return (String::new(), detail.to_string());
    }
    let (mut creator, mut location) = (Vec::new(), String::new());
    for part in detail.split(" · ") {
        if part.contains("://") {
            location = part.to_string();
        } else if !part.is_empty() {
            creator.push(part);
        }
    }
    (creator.join(" · "), location)
}

fn valid_kinds(kinds: &[String]) -> Vec<String> {
    kinds
        .iter()
        .filter(|k| KINDS.contains(&k.as_str()))
        .cloned()
        .collect()
}

pub struct Store {
    conn: Mutex<Connection>,
    path: Option<String>,
    clock: Clock,
}

type Res<T> = rusqlite::Result<T>;

impl Store {
    pub fn open(path: &Path, clock: Clock) -> Res<Self> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // Create the file 0600 before SQLite opens it.
        let _ = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path);
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        let conn = Connection::open(path)?;
        Self::init(conn, Some(path.to_string_lossy().into_owned()), clock)
    }

    #[cfg(test)]
    pub fn memory(clock: Clock) -> Res<Self> {
        Self::init(Connection::open_in_memory()?, None, clock)
    }

    fn init(conn: Connection, path: Option<String>, clock: Clock) -> Res<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "secure_delete", "ON")?;
        conn.execute_batch(SCHEMA)?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(Self {
            conn: Mutex::new(conn),
            path,
            clock,
        })
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    // -- writes --------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn insert(
        &self,
        kind: &str,
        source: &str,
        app: &str,
        title: &str,
        detail: &str,
        at: f64,
        end: Option<f64>,
    ) -> Res<i64> {
        let db = self.db();
        db.execute(
            "INSERT INTO activity(kind, source, app, title, detail, started_at, ended_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![kind, source, app, title, detail, at, end],
        )?;
        Ok(db.last_insert_rowid())
    }

    /// Record a page visit, or lengthen it if this visit was reported before.
    /// A browser reports a long visit several times with a growing duration;
    /// the visit is identified by its address and start time. Returns the row
    /// id, or None if the visit started before the latest forget.
    pub fn upsert_page(&self, v: &PageVisit) -> Res<Option<i64>> {
        let end = v.at + v.seconds.max(0.0);
        let detail: Vec<&str> = [v.site, v.author, v.url]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();
        let detail = detail.join(" · ");
        let meta = serde_json::json!({
            "url": v.url, "site": v.site, "author": v.author, "content": v.content,
        })
        .to_string();
        let now = self.now();
        let mut db = self.db();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // A visit that started before a forget is never written again, new or
        // not: a late report could otherwise carry reading from a forgotten
        // window into a surviving row. The companion starts a fresh visit.
        let forget_before: f64 = tx.query_row(
            "SELECT COALESCE((SELECT CAST(value AS REAL) FROM meta WHERE key = 'page_forget_before'), 0)",
            [],
            |r| r.get(0),
        )?;
        if v.at <= forget_before {
            return Ok(None);
        }
        let existing: Option<i64> = tx
            .query_row(
                "SELECT id FROM activity WHERE kind = 'page' AND started_at BETWEEN ?1 AND ?2
                 AND json_extract(meta, '$.url') = ?3 ORDER BY id LIMIT 1",
                params![v.at - 0.001, v.at + 0.001, v.url],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            tx.execute(
                "UPDATE activity SET ended_at = MAX(COALESCE(ended_at, started_at), ?1),
                 title = ?2, detail = ?3,
                 meta = json_set(?4, '$.seen', MAX(COALESCE(json_extract(meta, '$.seen'), 0), ?5))
                 WHERE id = ?6",
                params![end, v.title, detail, meta, now, id],
            )?;
            tx.commit()?;
            return Ok(Some(id));
        }
        tx.execute(
            "INSERT INTO activity(kind, source, app, title, detail, started_at, ended_at, meta)
             VALUES ('page', ?1, ?2, ?3, ?4, ?5, ?6, json_set(?7, '$.seen', ?8))",
            params![v.source, v.host, v.title, detail, v.at, end, meta, now],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(Some(id))
    }

    pub fn open_span(
        &self,
        kind: &str,
        source: &str,
        app: &str,
        title: &str,
        detail: &str,
        at: Option<f64>,
    ) -> Res<i64> {
        let at = at.unwrap_or_else(|| self.now());
        self.insert(kind, source, app, title, detail, at, None)
    }

    pub fn add_event(
        &self,
        kind: &str,
        source: &str,
        app: &str,
        title: &str,
        detail: &str,
        at: Option<f64>,
    ) -> Res<i64> {
        let at = at.unwrap_or_else(|| self.now());
        self.insert(kind, source, app, title, detail, at, Some(at))
    }

    /// End a span. Spans shorter than `min_seconds` are deleted. Returns true if kept.
    pub fn close_span(&self, id: i64, at: Option<f64>, min_seconds: f64) -> Res<bool> {
        let at = at.unwrap_or_else(|| self.now());
        let db = self.db();
        let started: Option<f64> = db
            .query_row(
                "SELECT started_at FROM activity WHERE id = ?1 AND ended_at IS NULL",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(started) = started else {
            return Ok(false);
        };
        if at - started < min_seconds {
            db.execute("DELETE FROM activity WHERE id = ?1", [id])?;
            return Ok(false);
        }
        db.execute(
            "UPDATE activity SET ended_at = ?1 WHERE id = ?2",
            params![at.max(started), id],
        )?;
        Ok(true)
    }

    pub fn close_all_open(&self, at: f64) -> Res<usize> {
        self.db().execute(
            "UPDATE activity SET ended_at = MAX(started_at, ?1) WHERE ended_at IS NULL",
            [at],
        )
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Res<()> {
        self.db().execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_meta(&self, key: &str) -> Option<String> {
        self.db()
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
    }

    /// Delete activity overlapping a window and/or from one app, or everything.
    pub fn forget(
        &self,
        start: Option<f64>,
        end: Option<f64>,
        app: Option<&str>,
        everything: bool,
    ) -> Result<usize, String> {
        let app = app.filter(|a| !a.is_empty());
        if !everything && start.is_none() && end.is_none() && app.is_none() {
            return Err("forget needs a time window, an app, or --all".into());
        }
        let mut clauses = vec!["1=1".to_string()];
        let mut args: Vec<SqlValue> = Vec::new();
        if !everything {
            if let Some(s) = start {
                // A page counts until its latest report, not just its start
                // plus the time it was in front.
                clauses.push(
                    "MAX(COALESCE(ended_at, ?), CASE WHEN json_valid(meta) THEN COALESCE(json_extract(meta, '$.seen'), 0) ELSE 0 END) >= ?"
                        .into(),
                );
                args.push(self.now().into());
                args.push(s.into());
            }
            if let Some(e) = end {
                clauses.push("started_at <= ?".into());
                args.push(e.into());
            }
            if let Some(a) = app {
                clauses.push("lower(app) = lower(?)".into());
                args.push(a.to_string().into());
            }
        }
        let mut db = self.db();
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let count = tx
            .execute(
                &format!("DELETE FROM activity WHERE {}", clauses.join(" AND ")),
                params_from_iter(args),
            )
            .map_err(|e| e.to_string())?;
        if everything || start.is_some() || end.is_some() || count > 0 {
            // Cached summaries may quote forgotten activity.
            tx.execute("DELETE FROM summaries", [])
                .map_err(|e| e.to_string())?;
        }
        // Store only a timestamp. No page visit that started before this
        // forget is written again: a late report cannot recreate a deleted
        // row or lengthen a surviving one.
        tx.execute(
            "INSERT INTO meta (key, value) VALUES ('page_forget_before', ?1)
             ON CONFLICT(key) DO UPDATE SET value = MAX(CAST(value AS REAL), ?1)",
            [self.now()],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        if everything {
            db.execute_batch("INSERT INTO activity_fts(activity_fts) VALUES ('rebuild'); VACUUM;")
                .map_err(|e| e.to_string())?;
        }
        Ok(count)
    }

    /// Enforce retention. Window focus history older than `history` goes,
    /// with the day summaries written from it; the library (media, pages,
    /// files, notes) goes after `library`. `None` keeps that tier forever.
    pub fn purge(
        &self,
        history: Option<f64>,
        library: Option<f64>,
        history_day: &str,
    ) -> Res<usize> {
        let db = self.db();
        let mut n = 0;
        if let Some(cutoff) = history {
            n += db.execute(
                &format!("DELETE FROM activity WHERE kind IN {HISTORY_KINDS} AND ended_at IS NOT NULL AND ended_at < ?1"),
                [cutoff],
            )?;
            db.execute("DELETE FROM summaries WHERE day < ?1", [history_day])?;
        }
        if let Some(cutoff) = library {
            n += db.execute(
                &format!("DELETE FROM activity WHERE kind NOT IN {HISTORY_KINDS} AND ended_at IS NOT NULL AND ended_at < ?1"),
                [cutoff],
            )?;
        }
        Ok(n)
    }

    pub fn put_summary(&self, day: &str, text: &str, model: &str) -> Res<()> {
        self.db().execute(
            "INSERT INTO summaries(day, scope, model, text, created_at) VALUES (?1, 'day', ?2, ?3, ?4)
             ON CONFLICT(day, scope) DO UPDATE SET model = excluded.model, text = excluded.text, created_at = excluded.created_at",
            params![day, model, text, self.now()],
        )?;
        Ok(())
    }

    // -- reads ---------------------------------------------------------------

    fn rows(&self, sql: &str, args: Vec<SqlValue>) -> Res<Vec<Row>> {
        let now = self.now();
        let db = self.db();
        let mut stmt = db.prepare(sql)?;
        let rows = stmt.query_map(params_from_iter(args), |r| {
            let started: f64 = r.get(6)?;
            let ended: Option<f64> = r.get(7)?;
            let end = ended.unwrap_or(now);
            Ok(Row {
                id: r.get(0)?,
                kind: r.get(1)?,
                source: r.get(2)?,
                app: r.get(3)?,
                title: r.get(4)?,
                detail: r.get(5)?,
                started_at: started,
                ended_at: end,
                ongoing: ended.is_none(),
                seconds: round1((end - started).max(0.0)),
            })
        })?;
        rows.collect()
    }

    /// WHERE clauses for a time window and kinds, over `activity a`.
    fn filters(&self, q: &Query) -> (Vec<String>, Vec<SqlValue>) {
        let now = self.now();
        let mut clauses = vec!["1=1".to_string()];
        let mut args: Vec<SqlValue> = Vec::new();
        if let Some(s) = q.start {
            clauses.push("COALESCE(a.ended_at, ?) >= ?".into());
            args.extend([now.into(), s.into()]);
        }
        if let Some(e) = q.end {
            clauses.push("a.started_at <= ?".into());
            args.push(e.into());
        }
        let kinds = valid_kinds(q.kinds);
        if !kinds.is_empty() {
            clauses.push(format!("a.kind IN ({})", vec!["?"; kinds.len()].join(",")));
            args.extend(kinds.into_iter().map(SqlValue::from));
        }
        (clauses, args)
    }

    /// The ways to match free text, most exact first: every word as a prefix,
    /// every word as a substring ("script" finds "JavaScript"), then the rows
    /// that match the most words.
    fn matchers(text: &str) -> Vec<(&'static str, String, Vec<SqlValue>)> {
        let mut out = Vec::new();
        let all = fts_query(text);
        if all.is_empty() {
            return out;
        }
        out.push((
            MATCH_ALL,
            "a.id IN (SELECT rowid FROM activity_fts WHERE activity_fts MATCH ?)".to_string(),
            vec![all.clone().into()],
        ));
        let patterns = like_patterns(text);
        if !patterns.is_empty() {
            let one = "(a.title LIKE ? ESCAPE '\\' OR a.detail LIKE ? ESCAPE '\\' OR a.app LIKE ? ESCAPE '\\')";
            out.push((
                MATCH_SUBSTRING,
                vec![one; patterns.len()].join(" AND "),
                patterns
                    .iter()
                    .flat_map(|p| std::iter::repeat_n(SqlValue::from(p.clone()), 3))
                    .collect(),
            ));
        }
        if all.contains(' ') {
            out.push((
                MATCH_ANY,
                "a.id IN (SELECT rowid FROM activity_fts WHERE activity_fts MATCH ? ORDER BY rank LIMIT 200)".to_string(),
                vec![all.replace("* \"", "* OR \"").into()],
            ));
        }
        out
    }

    pub fn get(&self, id: i64) -> Res<Option<Row>> {
        let rows = self.rows(
            &format!("SELECT {COLUMNS} FROM activity a WHERE a.id = ?"),
            vec![id.into()],
        )?;
        Ok(rows.into_iter().next())
    }

    /// Distinct things in a window, each with all its sightings folded
    /// together, most recently seen first. With `text`, only those that match;
    /// the second value says how exact the match was ("" without text).
    pub fn items(&self, text: &str, q: &Query) -> Res<(Vec<Item>, &'static str)> {
        let now = self.now();
        let run = |extra: Option<(String, Vec<SqlValue>)>| -> Res<Vec<Item>> {
            let (mut clauses, mut filter_args) = self.filters(q);
            let mut args: Vec<SqlValue> = vec![now.into()];
            if let Some((clause, a)) = extra {
                clauses.push(clause);
                filter_args.extend(a);
            }
            args.extend(filter_args);
            args.push((q.limit.max(1) as i64).into());
            let sql = format!(
                "SELECT g.kind, g.app, g.title, b.detail, b.meta, g.first, g.last, g.secs, g.n, g.last_id
                 FROM (SELECT a.kind AS kind, a.app AS app, a.title AS title, MIN(a.started_at) AS first,
                              MAX(a.started_at) AS last, SUM(COALESCE(a.ended_at, ?) - a.started_at) AS secs,
                              COUNT(*) AS n, MAX(a.id) AS last_id
                       FROM activity a WHERE {} GROUP BY a.kind, a.app, a.title) g
                 JOIN activity b ON b.id = g.last_id
                 ORDER BY g.last DESC, g.last_id DESC LIMIT ?",
                clauses.join(" AND ")
            );
            let db = self.db();
            let mut stmt = db.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(args), |r| {
                let kind: String = r.get(0)?;
                let detail: String = r.get(3)?;
                let meta: String = r.get(4)?;
                let meta: serde_json::Value = serde_json::from_str(&meta).unwrap_or_default();
                let text = |key: &str| meta[key].as_str().unwrap_or("").to_string();
                let (creator, location) = if kind == "page" {
                    (text("author"), text("url"))
                } else {
                    split_detail(&kind, &detail)
                };
                Ok(Item {
                    site: text("site"),
                    content: text("content"),
                    creator,
                    location,
                    app: r.get(1)?,
                    title: r.get(2)?,
                    first_seen: r.get(5)?,
                    last_seen: r.get(6)?,
                    seconds: round1(r.get::<_, f64>(7)?.max(0.0)),
                    times: r.get(8)?,
                    last_id: r.get(9)?,
                    kind,
                })
            })?;
            rows.collect()
        };
        if text.trim().is_empty() {
            return Ok((run(None)?, ""));
        }
        for (label, clause, args) in Self::matchers(text) {
            let found = run(Some((clause, args)))?;
            if !found.is_empty() {
                return Ok((found, label));
            }
        }
        Ok((Vec::new(), ""))
    }

    pub fn timeline(&self, q: &Query) -> Res<Vec<Row>> {
        let now = self.now();
        let (mut clauses, mut args) = self.filters(q);
        if q.min_seconds > 0.0 {
            clauses.push("(COALESCE(a.ended_at, ?) - a.started_at >= ? OR a.kind IN ('file','note','marker'))".into());
            args.extend([now.into(), q.min_seconds.into()]);
        }
        let order = if q.newest_first { "DESC" } else { "ASC" };
        args.push((q.limit.max(1) as i64).into());
        self.rows(
            &format!(
                "SELECT {COLUMNS} FROM activity a WHERE {} ORDER BY a.started_at {order}, a.id {order} LIMIT ?",
                clauses.join(" AND ")
            ),
            args,
        )
    }

    pub fn current(&self, kind: &str) -> Res<Option<Row>> {
        let rows = self.rows(
            &format!("SELECT {COLUMNS} FROM activity a WHERE a.kind = ? AND a.ended_at IS NULL ORDER BY a.started_at DESC LIMIT 1"),
            vec![kind.to_string().into()],
        )?;
        Ok(rows.into_iter().next())
    }

    /// Rows matching free text, newest first. Every word must match, as a
    /// prefix; if nothing does, as a substring anywhere in the row.
    pub fn search(&self, text: &str, q: &Query) -> Res<Vec<Row>> {
        for (label, clause, match_args) in Self::matchers(text) {
            if label == MATCH_ANY {
                break;
            }
            let (mut clauses, mut args) = self.filters(q);
            clauses.push(clause);
            args.extend(match_args);
            args.push((q.limit.max(1) as i64).into());
            let rows = self.rows(
                &format!(
                    "SELECT {COLUMNS} FROM activity a WHERE {} ORDER BY a.started_at DESC, a.id DESC LIMIT ?",
                    clauses.join(" AND ")
                ),
                args,
            )?;
            if !rows.is_empty() {
                return Ok(rows);
            }
        }
        Ok(Vec::new())
    }

    /// Seconds of focus per app inside [start, end], clipped to the window.
    pub fn usage(&self, start: f64, end: Option<f64>, limit: usize) -> Res<Vec<Usage>> {
        let now = self.now();
        let end = end.unwrap_or(now);
        let db = self.db();
        let mut stmt = db.prepare(
            "SELECT app, SUM(MIN(COALESCE(ended_at, ?1), ?2) - MAX(started_at, ?3)) AS secs, COUNT(*)
             FROM activity WHERE kind = 'focus' AND COALESCE(ended_at, ?1) >= ?3 AND started_at <= ?2
             GROUP BY app ORDER BY secs DESC LIMIT ?4",
        )?;
        let rows = stmt.query_map(params![now, end, start, limit.max(1) as i64], |r| {
            let secs: Option<f64> = r.get(1)?;
            Ok(Usage {
                app: r.get(0)?,
                seconds: round1(secs.unwrap_or(0.0).max(0.0)),
                spans: r.get(2)?,
            })
        })?;
        rows.collect()
    }

    pub fn stats(&self) -> Res<Stats> {
        let db = self.db();
        let mut stmt = db.prepare("SELECT kind, COUNT(*) FROM activity GROUP BY kind")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<Res<BTreeMap<_, _>>>()?;
        let oldest: Option<f64> =
            db.query_row("SELECT MIN(started_at) FROM activity", [], |r| r.get(0))?;
        let bytes = self.path.as_ref().map_or(0, |p| {
            ["", "-wal", "-shm"]
                .iter()
                .filter_map(|s| std::fs::metadata(format!("{p}{s}")).ok())
                .map(|m| m.len())
                .sum()
        });
        Ok(Stats {
            rows,
            oldest,
            bytes,
        })
    }

    pub fn get_summary(&self, day: &str) -> Option<Summary> {
        self.db()
            .query_row(
                "SELECT day, model, text, created_at FROM summaries WHERE day = ?1 AND scope = 'day'",
                [day],
                |r| Ok(Summary { day: r.get(0)?, model: r.get(1)?, text: r.get(2)?, created_at: r.get(3)? }),
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn export(&self) -> Res<Vec<Row>> {
        self.rows(
            &format!("SELECT {COLUMNS} FROM activity a ORDER BY a.started_at"),
            Vec::new(),
        )
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A settable clock for tests.
    #[derive(Clone)]
    pub struct TestClock(pub Arc<AtomicU64>);
    impl TestClock {
        pub fn new() -> Self {
            Self(Arc::new(AtomicU64::new(1_800_000_000_000)))
        }
        pub fn now(&self) -> f64 {
            self.0.load(Ordering::SeqCst) as f64 / 1000.0
        }
        pub fn tick(&self, secs: f64) {
            self.0.fetch_add((secs * 1000.0) as u64, Ordering::SeqCst);
        }
        pub fn clock(&self) -> Clock {
            let c = self.clone();
            Arc::new(move || c.now())
        }
    }

    fn all() -> Query<'static> {
        Query {
            start: Some(0.0),
            limit: 1000,
            ..Default::default()
        }
    }

    #[test]
    fn database_file_is_private() {
        let dir = std::env::temp_dir().join(format!("oc-store-{}", std::process::id()));
        let db = dir.join("x/cortex.db");
        Store::open(&db, TestClock::new().clock()).unwrap();
        assert_eq!(
            std::fs::metadata(&db).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn spans_open_close_and_short_ones_are_dropped() {
        let c = TestClock::new();
        let s = Store::memory(c.clock()).unwrap();
        let a = s
            .open_span("focus", "h", "kitty", "nvim", "", None)
            .unwrap();
        c.tick(1.0);
        assert!(!s.close_span(a, None, 2.0).unwrap());
        let b = s
            .open_span("focus", "h", "kitty", "nvim", "", None)
            .unwrap();
        c.tick(10.0);
        assert!(s.close_span(b, None, 2.0).unwrap());
        let rows = s.timeline(&all()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].seconds, 10.0);
        assert!(!rows[0].ongoing);
    }

    #[test]
    fn open_span_reads_as_ongoing() {
        let c = TestClock::new();
        let s = Store::memory(c.clock()).unwrap();
        s.open_span("focus", "h", "chromium", "Docs", "", None)
            .unwrap();
        c.tick(42.0);
        let cur = s.current("focus").unwrap().unwrap();
        assert!(cur.ongoing);
        assert_eq!(cur.seconds, 42.0);
    }

    #[test]
    fn search_matches_prefixes_and_survives_hostile_queries() {
        let s = Store::memory(TestClock::new().clock()).unwrap();
        s.add_event(
            "note",
            "cli",
            "",
            "Finished the quarterly report draft",
            "",
            None,
        )
        .unwrap();
        s.add_event("file", "f", "", "budget.ods", "/home/u/budget.ods", None)
            .unwrap();
        let q = Query {
            limit: 50,
            ..Default::default()
        };
        assert_eq!(
            s.search("quart rep", &q).unwrap()[0].title,
            "Finished the quarterly report draft"
        );
        assert_eq!(s.search("budget", &q).unwrap()[0].kind, "file");
        for nasty in [
            "\"",
            "NEAR(",
            "a OR",
            "*",
            ")",
            "title:",
            "'; DROP TABLE activity; --",
        ] {
            s.search(nasty, &q).unwrap();
        }
        assert_eq!(fts_query("  "), "");
    }

    fn visit<'a>(title: &'a str, url: &'a str, at: f64, seconds: f64) -> PageVisit<'a> {
        PageVisit {
            source: "browser:test",
            host: "example.com",
            title,
            url,
            site: "Example",
            author: "Ann Author",
            content: "article",
            at,
            seconds,
        }
    }

    #[test]
    fn items_fold_sightings_and_say_how_they_matched() {
        let c = TestClock::new();
        let s = Store::memory(c.clock()).unwrap();
        let t0 = c.now() - 5000.0;
        let url = "https://example.com/p/rates";
        // One visit reported twice, then a second visit the next hour.
        let first = s
            .upsert_page(&visit("Interest rates explained", url, t0, 60.0))
            .unwrap();
        let again = s
            .upsert_page(&visit("Interest rates explained", url, t0, 300.0))
            .unwrap();
        assert!(first.is_some());
        assert_eq!(first, again);
        s.upsert_page(&visit("Interest rates explained", url, t0 + 3600.0, 100.0))
            .unwrap();
        s.add_event(
            "media",
            "mpris",
            "spotify",
            "100% Pure",
            "Band · https://m.example/t",
            Some(t0),
        )
        .unwrap();
        s.add_event(
            "file",
            "f",
            "Text Editor",
            "rates.ods",
            "/home/u/rates.ods",
            Some(t0),
        )
        .unwrap();

        let (found, how) = s.items("interest rates", &all()).unwrap();
        assert_eq!(how, MATCH_ALL);
        assert_eq!(found.len(), 1);
        let item = &found[0];
        assert_eq!((item.times, item.seconds), (2, 400.0));
        assert_eq!((item.first_seen, item.last_seen), (t0, t0 + 3600.0));
        assert_eq!(
            (
                item.site.as_str(),
                item.creator.as_str(),
                item.location.as_str(),
                item.content.as_str()
            ),
            ("Example", "Ann Author", url, "article")
        );

        // Part of a word, which the prefix index cannot find.
        let (found, how) = s.items("nterest xplain", &all()).unwrap();
        assert_eq!((found.len(), how), (1, MATCH_SUBSTRING));
        // No row has both words: the closest ones are offered instead.
        let (found, how) = s.items("rates spotify", &all()).unwrap();
        assert_eq!(how, MATCH_ANY);
        assert_eq!(found.len(), 3);
        // LIKE wildcards in the question are ordinary characters.
        assert_eq!(s.items("100% pur", &all()).unwrap().0[0].title, "100% Pure");
        assert!(s.items("%", &all()).unwrap().0.is_empty());
        assert!(s.items("zzz", &all()).unwrap().0.is_empty());

        // Media and files keep their creator and location too.
        let (everything, how) = s.items("", &all()).unwrap();
        assert_eq!((everything.len(), how), (3, ""));
        let media = everything.iter().find(|i| i.kind == "media").unwrap();
        assert_eq!(
            (media.creator.as_str(), media.location.as_str()),
            ("Band", "https://m.example/t")
        );
        let file = everything.iter().find(|i| i.kind == "file").unwrap();
        assert_eq!(file.location, "/home/u/rates.ods");
        // Search (rows, not items) falls back to substrings as well.
        assert_eq!(s.search("xplain", &all()).unwrap().len(), 2);
    }

    #[test]
    fn usage_clips_spans_to_the_window() {
        let c = TestClock::new();
        let s = Store::memory(c.clock()).unwrap();
        let t0 = c.now();
        let a = s
            .open_span("focus", "h", "kitty", "", "", Some(t0))
            .unwrap();
        s.close_span(a, Some(t0 + 100.0), 0.0).unwrap();
        let b = s
            .open_span("focus", "h", "chromium", "", "", Some(t0 + 100.0))
            .unwrap();
        s.close_span(b, Some(t0 + 400.0), 0.0).unwrap();
        let u: BTreeMap<_, _> = s
            .usage(t0 + 50.0, Some(t0 + 200.0), 10)
            .unwrap()
            .into_iter()
            .map(|u| (u.app, u.seconds))
            .collect();
        assert_eq!(u.get("kitty"), Some(&50.0));
        assert_eq!(u.get("chromium"), Some(&100.0));
    }

    #[test]
    fn forget_and_retention() {
        let c = TestClock::new();
        let s = Store::memory(c.clock()).unwrap();
        let t0 = c.now();
        let old = s
            .open_span("focus", "h", "kitty", "", "", Some(t0 - 40.0 * 86400.0))
            .unwrap();
        s.close_span(old, Some(t0 - 40.0 * 86400.0 + 60.0), 0.0)
            .unwrap();
        s.add_event("note", "cli", "", "recent", "", Some(t0 - 60.0))
            .unwrap();
        s.add_event("note", "cli", "", "secret plan", "", Some(t0 - 5.0))
            .unwrap();
        // A track from 40 days ago is library, not history: it outlives focus.
        s.add_event(
            "media",
            "mpris",
            "mpv",
            "Old film",
            "",
            Some(t0 - 40.0 * 86400.0),
        )
        .unwrap();
        let cutoff = t0 - 30.0 * 86400.0;
        assert_eq!(
            s.purge(Some(cutoff), Some(t0 - 365.0 * 86400.0), "2020-01-01")
                .unwrap(),
            1
        );
        assert_eq!(s.search("film", &all()).unwrap().len(), 1);
        assert_eq!(s.purge(None, Some(cutoff), "").unwrap(), 1);
        assert_eq!(s.forget(Some(t0 - 30.0), None, None, false).unwrap(), 1);
        let titles: Vec<_> = s
            .timeline(&all())
            .unwrap()
            .into_iter()
            .map(|r| r.title)
            .collect();
        assert_eq!(titles, vec!["recent"]);
        assert!(s.search("secret", &all()).unwrap().is_empty());
        assert!(s.forget(None, None, None, false).is_err());
        assert_eq!(s.forget(None, None, None, true).unwrap(), 1);
        assert!(s.timeline(&all()).unwrap().is_empty());
    }

    #[test]
    fn forgetting_a_window_catches_a_page_read_in_it_long_after_it_opened() {
        let c = TestClock::new();
        let s = Store::memory(c.clock()).unwrap();
        let url = "https://example.com/p/rates";
        let opened = c.now();
        c.tick(30.0);
        s.upsert_page(&visit("Rates", url, opened, 30.0)).unwrap();
        // The tab sits in the background for two hours, then is read again:
        // its start plus its foreground seconds is still two hours ago.
        c.tick(7200.0);
        s.upsert_page(&visit("Rates, updated", url, opened, 60.0))
            .unwrap();
        s.put_summary("2027-01-15", "Read about rates", "m")
            .unwrap();
        let window = c.now() - 900.0;
        assert_eq!(s.forget(Some(window), None, None, false).unwrap(), 1);
        assert!(s.export().unwrap().is_empty());
        assert!(s.get_summary("2027-01-15").is_none());
        // The same visit's next report cannot bring it back.
        c.tick(60.0);
        assert_eq!(
            s.upsert_page(&visit("Rates", url, opened, 120.0)).unwrap(),
            None
        );
        assert!(s.export().unwrap().is_empty());
        // A page last seen before the window is kept.
        let earlier = c.now();
        c.tick(60.0);
        s.upsert_page(&visit("Earlier", url, earlier, 60.0))
            .unwrap();
        c.tick(3600.0);
        assert_eq!(
            s.forget(Some(c.now() - 900.0), None, None, false).unwrap(),
            0
        );
        assert_eq!(s.export().unwrap().len(), 1);
    }

    #[test]
    fn a_forget_stops_every_earlier_visit_from_growing() {
        let c = TestClock::new();
        let s = Store::memory(c.clock()).unwrap();
        let url = "https://example.com/p/rates";
        let opened = c.now();
        c.tick(60.0);
        s.upsert_page(&visit("Rates", url, opened, 60.0)).unwrap();
        s.put_summary("2027-01-15", "Read about rates", "m")
            .unwrap();
        // Forgetting another site leaves this row, and the summary, alone.
        assert_eq!(s.forget(None, None, Some("other.example"), false), Ok(0));
        assert!(s.get_summary("2027-01-15").is_some());
        // A late report of the surviving visit could carry reading from a
        // forgotten window into it, so it is refused rather than merged.
        c.tick(60.0);
        assert_eq!(
            s.upsert_page(&visit("Rates, retitled", url, opened, 120.0))
                .unwrap(),
            None
        );
        let rows = s.export().unwrap();
        assert_eq!(
            (rows.len(), rows[0].title.as_str(), rows[0].seconds),
            (1, "Rates", 60.0)
        );
        // The browser starts a fresh visit, which is stored.
        let fresh = c.now();
        c.tick(60.0);
        assert!(
            s.upsert_page(&visit("Rates", url, fresh, 60.0))
                .unwrap()
                .is_some()
        );
        // Forgetting a site that had rows drops summaries that may quote it.
        assert_eq!(s.forget(None, None, Some("example.com"), false), Ok(2));
        assert!(s.get_summary("2027-01-15").is_none());
    }

    #[test]
    fn forgetting_a_window_tolerates_rows_with_unreadable_meta() {
        let c = TestClock::new();
        let s = Store::memory(c.clock()).unwrap();
        s.db()
            .execute(
                "INSERT INTO activity(kind, source, app, title, started_at, ended_at, meta)
                 VALUES ('focus', 'h', 'kitty', 'old', ?1, ?1, 'not json')",
                [c.now() - 60.0],
            )
            .unwrap();
        assert_eq!(s.forget(Some(c.now() - 900.0), None, None, false), Ok(1));
    }

    #[test]
    fn forget_by_app_is_case_insensitive() {
        let s = Store::memory(TestClock::new().clock()).unwrap();
        s.add_event("focus", "h", "Signal", "chat", "", None)
            .unwrap();
        s.add_event("focus", "h", "kitty", "shell", "", None)
            .unwrap();
        assert_eq!(s.forget(None, None, Some("signal"), false).unwrap(), 1);
        assert_eq!(s.timeline(&all()).unwrap()[0].app, "kitty");
    }
}
