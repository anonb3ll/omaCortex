//! The activity store: one SQLite file with full-text search.
//!
//! Each `activity` row is either a span (focus on a window, a track playing)
//! with `started_at`/`ended_at`, or a point event (a file opened, a note)
//! whose `ended_at` equals `started_at`. A span still in progress has
//! `ended_at` NULL; readers treat it as ending now. The schema is the same one
//! the 0.1 Python recorder wrote, so existing databases keep working.

use crate::timeutil::Clock;
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::Mutex;

pub const KINDS: [&str; 5] = ["focus", "media", "file", "note", "marker"];
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
                clauses.push("COALESCE(ended_at, ?) >= ?".into());
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
        let db = self.db();
        let count = db
            .execute(
                &format!("DELETE FROM activity WHERE {}", clauses.join(" AND ")),
                params_from_iter(args),
            )
            .map_err(|e| e.to_string())?;
        if everything || start.is_some() || end.is_some() {
            // Cached summaries may quote forgotten activity.
            db.execute("DELETE FROM summaries", [])
                .map_err(|e| e.to_string())?;
        }
        if everything {
            db.execute_batch("INSERT INTO activity_fts(activity_fts) VALUES ('rebuild'); VACUUM;")
                .map_err(|e| e.to_string())?;
        }
        Ok(count)
    }

    pub fn purge_older_than(&self, cutoff: f64, cutoff_day: &str) -> Res<usize> {
        let db = self.db();
        let n = db.execute(
            "DELETE FROM activity WHERE ended_at IS NOT NULL AND ended_at < ?1",
            [cutoff],
        )?;
        db.execute("DELETE FROM summaries WHERE day < ?1", [cutoff_day])?;
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

    pub fn timeline(&self, q: &Query) -> Res<Vec<Row>> {
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

    pub fn search(&self, text: &str, q: &Query) -> Res<Vec<Row>> {
        let m = fts_query(text);
        if m.is_empty() {
            return Ok(Vec::new());
        }
        let now = self.now();
        let mut clauses = vec!["activity_fts MATCH ?".to_string()];
        let mut args: Vec<SqlValue> = vec![m.into()];
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
        args.push((q.limit.max(1) as i64).into());
        self.rows(
            &format!(
                "SELECT {COLUMNS} FROM activity_fts JOIN activity a ON a.id = activity_fts.rowid WHERE {} ORDER BY a.started_at DESC LIMIT ?",
                clauses.join(" AND ")
            ),
            args,
        )
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
        assert_eq!(
            s.purge_older_than(t0 - 30.0 * 86400.0, "2020-01-01")
                .unwrap(),
            1
        );
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
