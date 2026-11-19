//! `omacortex`: a private, local memory of what you do on your Omarchy desktop.

mod api;
mod browser;
mod collectors;
mod config;
mod daemon;
mod llm;
mod mcp;
mod privacy;
mod recorder;
mod sessions;
mod state;
mod store;
mod timeutil;

use api::{Cortex, Res};
use clap::{Parser, Subcommand};
use config::Config;
use serde_json::{Value, json};
use std::io::IsTerminal;
use store::{Item, Query, Row};
use timeutil::{clock, human_duration, parse_duration, parse_when};

#[derive(Parser)]
#[command(
    name = "omacortex",
    version,
    about = "A private, local memory of what you do on your Omarchy desktop."
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Kind {
    Focus,
    Media,
    Page,
    File,
    Note,
    Marker,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Focus => "focus",
            Kind::Media => "media",
            Kind::Page => "page",
            Kind::File => "file",
            Kind::Note => "note",
            Kind::Marker => "marker",
        }
    }
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Mark {
    Away,
    Back,
    Refresh,
    Ping,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the capture daemon (the shell plugin starts it for you)
    Daemon {
        /// Exit quietly if one is already running
        #[arg(long)]
        if_not_running: bool,
    },
    /// Show whether omaCortex is recording
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Give consent and start recording
    Enable,
    /// Stop recording (keeps history)
    Disable,
    /// Pause recording, optionally for a while (15m, 1h)
    Pause { duration: Option<String> },
    /// Resume recording
    Resume,
    /// Pause or resume
    Toggle,
    /// What you are doing and were just doing
    Now {
        #[arg(long, default_value_t = 30)]
        minutes: u32,
        #[arg(long)]
        json: bool,
    },
    /// Raw activity in a time range
    Timeline {
        #[arg(long, default_value = "1h")]
        since: String,
        #[arg(long)]
        until: Option<String>,
        #[arg(long, value_enum)]
        kind: Vec<Kind>,
        #[arg(long, default_value_t = 0.0)]
        min_seconds: f64,
        #[arg(long, default_value_t = 200)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Focus grouped into sessions
    Sessions {
        #[arg(long, default_value = "today")]
        since: String,
        #[arg(long)]
        until: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Full-text search over everything recorded
    Search {
        #[arg(required = true)]
        query: Vec<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long, value_enum)]
        kind: Vec<Kind>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Find something you half remember: "that article about rates last week"
    Recall {
        #[arg(required = true)]
        question: Vec<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// What you watched, listened to and read, each thing listed once
    Library {
        /// Only things matching these words
        query: Vec<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        until: Option<String>,
        /// Default: media and page
        #[arg(long, value_enum)]
        kind: Vec<Kind>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Everything going on around one moment
    Around {
        /// A time: 'yesterday', '2h', 2026-11-19T09:00 (default: now)
        when: Option<String>,
        /// Around a row instead, by the id that recall and search print
        #[arg(long, conflicts_with = "when")]
        id: Option<i64>,
        /// Minutes either side
        #[arg(long, default_value_t = 10)]
        minutes: u32,
        #[arg(long)]
        json: bool,
    },
    /// Connect a browser, so pages you read and watch are remembered by name
    Browser {
        #[command(subcommand)]
        action: BrowserCmd,
    },
    /// Time per app
    Usage {
        #[arg(long, default_value = "today")]
        since: String,
        #[arg(long)]
        until: Option<String>,
        #[arg(long, default_value_t = 25)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Add a note or checkpoint to your timeline
    Note {
        #[arg(required = true)]
        text: Vec<String>,
    },
    /// Delete recorded activity
    Forget {
        /// The last 15m, 2h, ...
        #[arg(long)]
        last: Option<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        until: Option<String>,
        /// Everything from one app (window class)
        #[arg(long)]
        app: Option<String>,
        /// Everything
        #[arg(long)]
        all: bool,
        /// Do not ask
        #[arg(long)]
        yes: bool,
    },
    /// Summarize a day with your local model
    Summarize {
        #[arg(default_value = "today")]
        day: String,
        /// Just print the digest
        #[arg(long)]
        no_model: bool,
        /// Ignore the cached summary
        #[arg(long)]
        refresh: bool,
        #[arg(long)]
        json: bool,
    },
    /// Ask your local model about your activity
    Ask {
        #[arg(required = true)]
        question: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Run the MCP server on stdio for agents
    Mcp,
    /// Data for the shell panel (internal)
    PanelJson,
    /// Tell the daemon you are away or back (used by the shell plugin)
    Mark {
        #[arg(value_enum)]
        what: Mark,
    },
    /// Print everything as JSON lines
    Export,
    /// Show the effective configuration
    Config {
        /// Write a commented config file
        #[arg(long)]
        init: bool,
        /// Print the config file path
        #[arg(long)]
        path: bool,
    },
    /// Check sensors, model and permissions
    Doctor {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum BrowserCmd {
    /// Let the omaCortex extension (and any --extension-id) talk to omaCortex
    Install {
        /// Also allow this extension id (32 letters, from chrome://extensions)
        #[arg(long)]
        extension_id: Vec<String>,
        /// Only these browsers: chromium, chrome, brave, vivaldi, edge
        #[arg(long)]
        browser: Vec<String>,
    },
    /// Remove the connection from every browser
    Uninstall,
    /// Which browsers are connected
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Print the folder to pick in "Load unpacked"
    Path,
}

fn print_json(v: &impl serde::Serialize) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn row_line(r: &Row) -> String {
    let span = if matches!(r.kind.as_str(), "file" | "note" | "marker") {
        String::new()
    } else {
        format!(" {:>6}", human_duration(r.seconds))
    };
    let who = if r.app.is_empty() {
        String::new()
    } else {
        format!("{}: ", r.app)
    };
    let flag = if r.ongoing { " (now)" } else { "" };
    format!(
        "{}{span}  [{}] {who}{}{flag}",
        timeutil::stamp(r.started_at),
        r.kind,
        r.title
    )
}

/// "video  Title — Channel · youtube.com  (2×, 43m)" and, below it, where it is.
fn item_lines(i: &Item) -> String {
    let what = if i.content.is_empty() {
        i.kind.as_str()
    } else {
        i.content.as_str()
    };
    let from: Vec<&str> = [
        i.creator.as_str(),
        if i.site.is_empty() { &i.app } else { &i.site },
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect();
    let from = if from.is_empty() {
        String::new()
    } else {
        format!(" — {}", from.join(" · "))
    };
    let seen = if i.times > 1 {
        format!("{}×, ", i.times)
    } else {
        String::new()
    };
    let length = if matches!(i.kind.as_str(), "file" | "note") {
        seen.trim_end_matches(", ").to_string()
    } else {
        format!("{seen}{}", human_duration(i.seconds))
    };
    let length = if length.is_empty() {
        String::new()
    } else {
        format!("  ({length})")
    };
    let mut out = format!(
        "{}  {what:<7} {}{from}{length}",
        timeutil::stamp(i.last_seen),
        i.title
    );
    if !i.location.is_empty() {
        out.push_str(&format!("\n{:20}{}  [id {}]", "", i.location, i.last_id));
    }
    out
}

fn kinds(k: &[Kind]) -> Vec<String> {
    k.iter().map(|k| k.name().to_string()).collect()
}

/// Tell a running daemon to re-read state now; fine if none is running.
fn poke(cfg: &Config) {
    let _ = daemon::send_control(cfg, "refresh");
}

fn open() -> Res<Cortex> {
    Cortex::open(Config::load(), timeutil::system_clock())
}

fn run(cmd: Cmd) -> Res<i32> {
    match cmd {
        Cmd::Daemon { if_not_running } => Ok(daemon::run(Config::load(), if_not_running)),
        Cmd::Status { json } => {
            let s = open()?.status()?;
            if json {
                print_json(&s);
                return Ok(0);
            }
            let state = if s["consent"] != true {
                "off: not enabled yet (run `omacortex enable`)".to_string()
            } else if s["paused"] == true {
                match s["paused_until"].as_f64() {
                    Some(t) => format!("paused until {}", clock(t)),
                    None => "paused".into(),
                }
            } else if s["away"] == true {
                "recording (you are away)".into()
            } else {
                "recording".into()
            };
            println!("omaCortex {}: {state}", s["version"].as_str().unwrap_or(""));
            println!(
                "daemon: {}",
                if s["daemon"] == true {
                    "running"
                } else {
                    "not running"
                }
            );
            let on: Vec<&str> = ["windows", "media", "files", "browser"]
                .into_iter()
                .filter(|k| s["sources"][k] == true)
                .collect();
            println!(
                "sensors: {}",
                if on.is_empty() {
                    "none".into()
                } else {
                    on.join(", ")
                }
            );
            let rows: i64 = s["rows"]
                .as_object()
                .map(|m| m.values().filter_map(Value::as_i64).sum())
                .unwrap_or(0);
            println!(
                "stored: {rows} rows ({} KiB), since {}",
                s["db_bytes"].as_u64().unwrap_or(0) / 1024,
                s["oldest"].as_str().unwrap_or("-")
            );
            let days = |key: &str| match s[key].as_u64() {
                Some(0) | None => "forever".to_string(),
                Some(d) => format!("{d} days"),
            };
            println!(
                "retention: window history {}, library (media, pages, files, notes) {}",
                days("retention_days"),
                days("library_days")
            );
            if let Some(e) = s["config_error"].as_str().filter(|e| !e.is_empty()) {
                println!("config error (defaults in use): {e}");
            }
            Ok(0)
        }
        Cmd::Enable => {
            let c = open()?;
            c.state.grant()?;
            c.state.resume()?;
            poke(&c.cfg);
            println!(
                "omaCortex is recording. Everything stays in {}",
                c.cfg.paths.data_dir.display()
            );
            println!("Pause any time with `omacortex pause` or the bar button.");
            Ok(0)
        }
        Cmd::Disable => {
            let c = open()?;
            c.state.revoke()?;
            poke(&c.cfg);
            println!(
                "omaCortex stopped recording. Your history is kept; `omacortex forget --all` deletes it."
            );
            Ok(0)
        }
        Cmd::Pause { duration } => {
            let c = open()?;
            let secs = duration.as_deref().map(parse_duration).transpose()?;
            c.state.pause(secs)?;
            poke(&c.cfg);
            println!(
                "paused{}",
                secs.map(|s| format!(" for {}", human_duration(s)))
                    .unwrap_or_else(|| " until you resume".into())
            );
            Ok(0)
        }
        Cmd::Resume => {
            let c = open()?;
            c.state.resume()?;
            poke(&c.cfg);
            println!(
                "{}",
                if c.state.read().consent {
                    "recording"
                } else {
                    "resumed, but not enabled: run `omacortex enable`"
                }
            );
            Ok(0)
        }
        Cmd::Toggle => {
            let rec = open()?.state.read();
            if !rec.consent {
                run(Cmd::Enable)
            } else if rec.paused {
                run(Cmd::Resume)
            } else {
                run(Cmd::Pause { duration: None })
            }
        }
        Cmd::Now { minutes, json } => {
            let v = open()?.now(minutes as f64)?;
            if json {
                print_json(&v);
                return Ok(0);
            }
            match v["current"].as_object() {
                Some(cur) => println!(
                    "Now: {}: {} (for {})",
                    cur["app"].as_str().unwrap_or(""),
                    cur["title"].as_str().unwrap_or(""),
                    human_duration(cur["seconds"].as_f64().unwrap_or(0.0))
                ),
                None => println!(
                    "Now: nothing focused{}",
                    if v["recording"] == true {
                        ""
                    } else {
                        " (not recording)"
                    }
                ),
            }
            for m in v["playing"].as_array().into_iter().flatten() {
                println!(
                    "Playing: {} [{}]",
                    m["title"].as_str().unwrap_or(""),
                    m["app"].as_str().unwrap_or("")
                );
            }
            let sessions = v["recent_sessions"].as_array().cloned().unwrap_or_default();
            if !sessions.is_empty() {
                println!("\nLast {minutes} minutes:");
                for s in sessions {
                    println!(
                        "  {}  {:>6}  {}: {}",
                        clock(s["started_at"].as_f64().unwrap_or(0.0)),
                        human_duration(s["active_seconds"].as_f64().unwrap_or(0.0)),
                        s["app"].as_str().unwrap_or(""),
                        s["title"].as_str().unwrap_or("")
                    );
                }
            }
            for n in v["recent_notes"].as_array().into_iter().flatten() {
                println!(
                    "Note {}: {}",
                    clock(n["started_at"].as_f64().unwrap_or(0.0)),
                    n["title"].as_str().unwrap_or("")
                );
            }
            let b = &v["before_break"];
            if let Some(away) = b["away_at"].as_f64() {
                println!("\nBefore you stepped away at {}:", clock(away));
                for s in b["sessions"].as_array().into_iter().flatten() {
                    println!(
                        "  {}  {:>6}  {}: {}",
                        clock(s["started_at"].as_f64().unwrap_or(0.0)),
                        human_duration(s["active_seconds"].as_f64().unwrap_or(0.0)),
                        s["app"].as_str().unwrap_or(""),
                        s["title"].as_str().unwrap_or("")
                    );
                }
            }
            Ok(0)
        }
        Cmd::Timeline {
            since,
            until,
            kind,
            min_seconds,
            limit,
            json,
        } => {
            let c = open()?;
            let now = c.now_ts();
            let k = kinds(&kind);
            let rows = c.timeline(&Query {
                start: parse_when(Some(&since), now)?,
                end: parse_when(until.as_deref(), now)?,
                kinds: &k,
                min_seconds,
                limit,
                newest_first: false,
            })?;
            if json {
                print_json(&rows)
            } else {
                rows.iter().for_each(|r| println!("{}", row_line(r)))
            }
            Ok(0)
        }
        Cmd::Sessions {
            since,
            until,
            limit,
            json,
        } => {
            let c = open()?;
            let now = c.now_ts();
            let rows = c.sessions(
                parse_when(Some(&since), now)?,
                parse_when(until.as_deref(), now)?,
                limit,
            )?;
            if json {
                print_json(&rows);
                return Ok(0);
            }
            for s in rows {
                let extra = if s.interruptions > 0 {
                    format!("  (+{} detours)", s.interruptions)
                } else {
                    String::new()
                };
                println!(
                    "{}-{}  {:>6}  {}: {}{extra}",
                    clock(s.started_at),
                    clock(s.ended_at),
                    human_duration(s.active_seconds),
                    s.app,
                    s.title
                );
            }
            Ok(0)
        }
        Cmd::Search {
            query,
            since,
            kind,
            limit,
            json,
        } => {
            let c = open()?;
            let k = kinds(&kind);
            let rows = c.store.search(
                &query.join(" "),
                &Query {
                    start: parse_when(since.as_deref(), c.now_ts())?,
                    kinds: &k,
                    limit,
                    ..Default::default()
                },
            )?;
            if json {
                print_json(&rows);
            } else if rows.is_empty() {
                println!("no matches");
            } else {
                rows.iter().for_each(|r| println!("{}", row_line(r)));
            }
            Ok(0)
        }
        Cmd::Recall {
            question,
            limit,
            json,
        } => {
            let c = open()?;
            let question = question.join(" ");
            if json {
                print_json(&c.recall(&question, limit)?);
                return Ok(0);
            }
            let r = c.recall_items(&question, limit)?;
            if r.items.is_empty() {
                println!("nothing found");
                return Ok(1);
            }
            let mut read_as = vec![format!("looked for {:?}", r.terms)];
            if let Some((start, end)) = r.range {
                read_as.push(format!(
                    "between {} and {}",
                    timeutil::stamp(start),
                    timeutil::stamp(end)
                ));
            }
            match r.matched {
                store::MATCH_SUBSTRING => read_as.push("matched parts of words".into()),
                store::MATCH_ANY => read_as.push("no row had every word; closest shown".into()),
                _ => {}
            }
            if r.widened {
                read_as.push("nothing of that kind, so searched everything".into());
            }
            println!("({})", read_as.join("; "));
            r.items.iter().for_each(|i| println!("{}", item_lines(i)));
            Ok(0)
        }
        Cmd::Library {
            query,
            since,
            until,
            kind,
            limit,
            json,
        } => {
            let c = open()?;
            let now = c.now_ts();
            let (items, matched) = c.library(
                &query.join(" "),
                &kinds(&kind),
                parse_when(since.as_deref(), now)?,
                parse_when(until.as_deref(), now)?,
                limit,
            )?;
            if json {
                print_json(&json!({"match": matched, "items": api::iso_items(&items)}));
            } else if items.is_empty() {
                println!("nothing yet");
            } else {
                items.iter().for_each(|i| println!("{}", item_lines(i)));
            }
            Ok(0)
        }
        Cmd::Around {
            when,
            id,
            minutes,
            json,
        } => {
            let c = open()?;
            let at = match id {
                Some(id) => {
                    c.store
                        .get(id)?
                        .ok_or(format!("no row with id {id}"))?
                        .started_at
                }
                None => parse_when(when.as_deref(), c.now_ts())?.unwrap_or(c.now_ts()),
            };
            let rows = c.around(at, minutes as f64, 200)?;
            if json {
                print_json(&rows);
            } else if rows.is_empty() {
                println!("nothing recorded around {}", timeutil::stamp(at));
            } else {
                rows.iter().for_each(|r| println!("{}", row_line(r)));
            }
            Ok(0)
        }
        Cmd::Browser { action } => browser_command(action),
        Cmd::Usage {
            since,
            until,
            limit,
            json,
        } => {
            let c = open()?;
            let now = c.now_ts();
            let start = parse_when(Some(&since), now)?.unwrap_or(0.0);
            let rows = c
                .store
                .usage(start, parse_when(until.as_deref(), now)?, limit)?;
            if json {
                print_json(&rows);
            } else {
                for u in rows {
                    println!(
                        "{:>7}  {}",
                        human_duration(u.seconds),
                        if u.app.is_empty() {
                            "(unknown)"
                        } else {
                            &u.app
                        }
                    );
                }
            }
            Ok(0)
        }
        Cmd::Note { text } => {
            let c = open()?;
            let rec = recorder::Recorder::new(
                c.store.clone(),
                c.privacy.clone(),
                c.state.clone(),
                0.0,
                c.clock.clone(),
            );
            if rec.note(&text.join(" "), "cli", None).is_none() {
                println!("empty note ignored");
                return Ok(1);
            }
            println!("noted");
            Ok(0)
        }
        Cmd::Forget {
            last,
            since,
            until,
            app,
            all,
            yes,
        } => {
            let c = open()?;
            let now = c.now_ts();
            let (scope, start, end, app, everything) = if all {
                (
                    "everything omaCortex has recorded".to_string(),
                    None,
                    None,
                    None,
                    true,
                )
            } else if let Some(l) = last {
                let secs = parse_duration(&l)?;
                (
                    format!("the last {}", human_duration(secs)),
                    Some(now - secs),
                    None,
                    None,
                    false,
                )
            } else if since.is_some() || until.is_some() || app.is_some() {
                (
                    "matching activity".into(),
                    parse_when(since.as_deref(), now)?,
                    parse_when(until.as_deref(), now)?,
                    app,
                    false,
                )
            } else {
                eprintln!("say what to forget: --last 15m, --since/--until, --app NAME, or --all");
                return Ok(2);
            };
            if !yes {
                if !std::io::stdin().is_terminal() {
                    eprintln!("refusing without --yes when not interactive");
                    return Ok(2);
                }
                eprint!("Delete {scope}? This cannot be undone. [y/N] ");
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
                    println!("kept");
                    return Ok(1);
                }
            }
            let n = c.store.forget(start, end, app.as_deref(), everything)?;
            println!("forgot {n} row(s)");
            Ok(0)
        }
        Cmd::Summarize {
            day,
            no_model,
            refresh,
            json,
        } => {
            let r = open()?.summary(&day, !no_model, refresh)?;
            if json {
                print_json(&r);
                return Ok(0);
            }
            println!("{}", r["text"].as_str().unwrap_or(""));
            if let Some(m) = r["model"].as_str().filter(|m| !m.is_empty()) {
                println!(
                    "\n({}{m})",
                    if r["cached"] == true { "cached, " } else { "" }
                );
            } else if let Some(e) = r["model_error"].as_str() {
                eprintln!("\n(no local model: {e})");
            }
            Ok(0)
        }
        Cmd::Ask { question, json } => match open()?.ask(&question.join(" ")) {
            Ok(r) => {
                if json {
                    print_json(&r)
                } else {
                    println!("{}", r["answer"].as_str().unwrap_or(""))
                }
                Ok(0)
            }
            Err(e) => {
                eprintln!("no local model: {e}");
                Ok(1)
            }
        },
        Cmd::Mcp => {
            let c = open()?;
            mcp::serve(&c, std::io::stdin().lock(), std::io::stdout().lock())?;
            Ok(0)
        }
        Cmd::PanelJson => {
            println!("{}", open()?.panel()?);
            Ok(0)
        }
        Cmd::Mark { what } => {
            let cmd = match what {
                Mark::Away => "away",
                Mark::Back => "back",
                Mark::Refresh => "refresh",
                Mark::Ping => "ping",
            };
            match daemon::send_control(&Config::load(), cmd) {
                Ok(v) => {
                    println!("{v}");
                    Ok(0)
                }
                Err(e) => {
                    eprintln!("daemon not reachable: {e}");
                    Ok(1)
                }
            }
        }
        Cmd::Export => {
            for r in open()?.store.export()? {
                let mut v = serde_json::to_value(&r)?;
                v["started_at"] = json!(timeutil::iso(r.started_at));
                v["ended_at"] = json!(timeutil::iso(r.ended_at));
                println!("{v}");
            }
            Ok(0)
        }
        Cmd::Config { init, path } => {
            let cfg = Config::load();
            let file = cfg.paths.config_file();
            if init {
                if file.exists() {
                    eprintln!("{} already exists", file.display());
                    return Ok(1);
                }
                std::fs::create_dir_all(&cfg.paths.config_dir)?;
                std::fs::write(&file, config::EXAMPLE_CONFIG)?;
                println!("wrote {}", file.display());
            } else if path {
                println!("{}", file.display());
            } else {
                print_json(&cfg.settings);
            }
            Ok(0)
        }
        Cmd::Doctor { json } => doctor(json),
    }
}

fn browser_command(action: BrowserCmd) -> Res<i32> {
    let base = browser::config_base();
    match action {
        BrowserCmd::Install {
            extension_id,
            browser,
        } => {
            let program = browser::host_program();
            let written = browser::install(&base, &program, &extension_id, &browser)?;
            if written.is_empty() {
                eprintln!(
                    "no Chromium-family browser found under {}; name one with --browser",
                    base.display()
                );
                return Ok(1);
            }
            for path in &written {
                println!("wrote {}", path.display());
            }
            println!("The browser will start: {}", program.display());
            match browser::extension_dir() {
                Some(dir) => println!(
                    "Now load the extension: open chrome://extensions, turn on Developer mode, \
                     choose \"Load unpacked\" and pick\n  {}",
                    dir.display()
                ),
                None => println!(
                    "Now load the extension from the `extension` folder of the omaCortex plugin."
                ),
            }
            println!("Pages are recorded only while omaCortex is recording (`omacortex status`).");
            Ok(0)
        }
        BrowserCmd::Uninstall => {
            let removed = browser::uninstall(&base);
            for path in &removed {
                println!("removed {}", path.display());
            }
            if removed.is_empty() {
                println!("no browser was connected");
            }
            Ok(0)
        }
        BrowserCmd::Status { json } => {
            let all = browser::status(&base);
            if json {
                print_json(&all);
                return Ok(0);
            }
            for s in all.iter().filter(|s| s.present || s.installed) {
                let state = if !s.installed {
                    "not connected (run `omacortex browser install`)".to_string()
                } else if !s.program_ok {
                    format!("connected, but {} is missing; reinstall", s.program)
                } else {
                    format!("connected for {} extension(s)", s.extensions.len())
                };
                println!("{:<9} {state}", s.browser);
            }
            if !all.iter().any(|s| s.present || s.installed) {
                println!("no Chromium-family browser found");
            }
            Ok(0)
        }
        BrowserCmd::Path => match browser::extension_dir() {
            Some(dir) => {
                println!("{}", dir.display());
                Ok(0)
            }
            None => {
                eprintln!("the extension folder was not found next to this program");
                Ok(1)
            }
        },
    }
}

fn doctor(as_json: bool) -> Res<i32> {
    use std::os::unix::fs::PermissionsExt;
    let cfg = Config::load();
    let mut checks: Vec<(String, bool, String)> = Vec::new();
    let mut add = |name: &str, ok: bool, detail: String| checks.push((name.into(), ok, detail));
    add(
        "config",
        cfg.error.is_empty(),
        if cfg.error.is_empty() {
            cfg.paths.config_file().display().to_string()
        } else {
            cfg.error.clone()
        },
    );
    let bad = privacy::Privacy::from_settings(&cfg.settings.privacy).bad_patterns;
    add(
        "privacy patterns",
        bad.is_empty(),
        if bad.is_empty() {
            "all valid".into()
        } else {
            bad.join(", ")
        },
    );
    let dir = collectors::hyprland::socket_dir();
    add(
        "hyprland socket",
        dir.is_some(),
        if dir.is_some() {
            "found".into()
        } else {
            "no running Hyprland instance".into()
        },
    );
    let busctl = collectors::media::available();
    add(
        "busctl (media)",
        busctl,
        if busctl {
            format!(
                "{} player(s) on the bus",
                collectors::media::list_players().len()
            )
        } else {
            "missing".into()
        },
    );
    let connected: Vec<String> = browser::status(&browser::config_base())
        .into_iter()
        .filter(|s| s.installed && s.program_ok)
        .map(|s| s.browser)
        .collect();
    add(
        "browser pages",
        !connected.is_empty(),
        if connected.is_empty() {
            "optional: run `omacortex browser install`".into()
        } else {
            connected.join(", ")
        },
    );
    match llm::resolve(&cfg.settings.model) {
        Ok(m) => add(
            "local model",
            true,
            format!("{} at {}", m.model, m.endpoint),
        ),
        Err(e) => add("local model", false, format!("optional: {e}")),
    }
    let c = Cortex::open(cfg, timeutil::system_clock())?;
    let s = c.status()?;
    add(
        "consent",
        s["consent"] == true,
        if s["consent"] == true {
            "enabled".into()
        } else {
            "run `omacortex enable`".into()
        },
    );
    add(
        "daemon",
        s["daemon"] == true,
        if s["daemon"] == true {
            "running".into()
        } else {
            "not running".into()
        },
    );
    let mode = std::fs::metadata(c.cfg.paths.db())?.permissions().mode() & 0o777;
    add(
        "database permissions",
        mode == 0o600,
        format!("{} {mode:o}", c.cfg.paths.db().display()),
    );
    if as_json {
        print_json(
            &checks
                .iter()
                .map(|(n, ok, d)| json!({"check": n, "ok": ok, "detail": d}))
                .collect::<Vec<_>>(),
        );
    } else {
        for (n, ok, d) in &checks {
            println!("{} {n:<22} {d}", if *ok { "ok " } else { "!! " });
        }
    }
    Ok(0)
}

/// Chrome starts a native-messaging host with the calling extension's origin
/// as its first argument. In that mode stdout carries the protocol, so
/// nothing else may print there.
fn browser_host() -> i32 {
    let served = open().and_then(|c| {
        browser::serve(&c, std::io::stdin().lock(), std::io::stdout().lock())?;
        Ok(())
    });
    match served {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("omacortex: {e}");
            let _ = browser::write_message(
                &mut std::io::stdout().lock(),
                &json!({"ok": false, "error": "omacortex could not open its store"}),
            );
            1
        }
    }
}

fn main() {
    if std::env::args()
        .nth(1)
        .is_some_and(|a| a.starts_with("chrome-extension://"))
    {
        std::process::exit(browser_host());
    }
    let cli = Cli::parse();
    let code = match run(cli.command) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("omacortex: {e}");
            2
        }
    };
    std::process::exit(code);
}
