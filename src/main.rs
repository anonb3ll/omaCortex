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

/// The one-line recording state printed by `omacortex status`.
fn recording_status_line(
    consent: bool,
    paused: bool,
    paused_until: Option<f64>,
    away: bool,
) -> String {
    if !consent {
        "off: not enabled yet (run `omacortex enable`)".to_string()
    } else if paused {
        match paused_until {
            Some(t) => format!("paused until {}", clock(t)),
            None => "paused".into(),
        }
    } else if away {
        "recording (you are away)".into()
    } else {
        "recording".into()
    }
}

fn enabled_sensors(windows: bool, media: bool, files: bool, browser: bool) -> String {
    let on: Vec<&str> = [
        ("windows", windows),
        ("media", media),
        ("files", files),
        ("browser", browser),
    ]
    .into_iter()
    .filter(|(_, on)| *on)
    .map(|(name, _)| name)
    .collect();
    if on.is_empty() {
        "none".into()
    } else {
        on.join(", ")
    }
}

fn retention_phrase(days: Option<u64>) -> String {
    match days {
        Some(0) | None => "forever".to_string(),
        Some(d) => format!("{d} days"),
    }
}

struct ForgetPlan {
    scope: String,
    start: Option<f64>,
    end: Option<f64>,
    app: Option<String>,
    everything: bool,
}

enum ForgetChoice {
    Ready(ForgetPlan),
    MissingTarget,
}

/// What `forget` will delete. `--all` wins over every other filter.
fn plan_forget(
    now: f64,
    last: Option<&str>,
    since: Option<&str>,
    until: Option<&str>,
    app: Option<String>,
    all: bool,
) -> Result<ForgetChoice, String> {
    if all {
        return Ok(ForgetChoice::Ready(ForgetPlan {
            scope: "everything omaCortex has recorded".to_string(),
            start: None,
            end: None,
            app: None,
            everything: true,
        }));
    }
    if let Some(last) = last {
        let secs = parse_duration(last)?;
        return Ok(ForgetChoice::Ready(ForgetPlan {
            scope: format!("the last {}", human_duration(secs)),
            start: Some(now - secs),
            end: None,
            app: None,
            everything: false,
        }));
    }
    if since.is_some() || until.is_some() || app.is_some() {
        return Ok(ForgetChoice::Ready(ForgetPlan {
            scope: "matching activity".into(),
            start: parse_when(since, now)?,
            end: parse_when(until, now)?,
            app,
            everything: false,
        }));
    }
    Ok(ForgetChoice::MissingTarget)
}

fn browser_connection_line(
    installed: bool,
    program_ok: bool,
    program: &str,
    extensions: usize,
) -> String {
    if !installed {
        "not connected (run `omacortex browser install`)".to_string()
    } else if !program_ok {
        format!("connected, but {program} is missing; reinstall")
    } else {
        format!("connected for {extensions} extension(s)")
    }
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
            let state = recording_status_line(
                s["consent"] == true,
                s["paused"] == true,
                s["paused_until"].as_f64(),
                s["away"] == true,
            );
            println!("omaCortex {}: {state}", s["version"].as_str().unwrap_or(""));
            println!(
                "daemon: {}",
                if s["daemon"] == true {
                    "running"
                } else {
                    "not running"
                }
            );
            println!(
                "sensors: {}",
                enabled_sensors(
                    s["sources"]["windows"] == true,
                    s["sources"]["media"] == true,
                    s["sources"]["files"] == true,
                    s["sources"]["browser"] == true,
                )
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
            println!(
                "retention: window history {}, library (media, pages, files, notes) {}",
                retention_phrase(s["retention_days"].as_u64()),
                retention_phrase(s["library_days"].as_u64())
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
            let plan = match plan_forget(
                now,
                last.as_deref(),
                since.as_deref(),
                until.as_deref(),
                app,
                all,
            )? {
                ForgetChoice::MissingTarget => {
                    eprintln!(
                        "say what to forget: --last 15m, --since/--until, --app NAME, or --all"
                    );
                    return Ok(2);
                }
                ForgetChoice::Ready(plan) => plan,
            };
            if !yes {
                if !std::io::stdin().is_terminal() {
                    eprintln!("refusing without --yes when not interactive");
                    return Ok(2);
                }
                eprint!("Delete {}? This cannot be undone. [y/N] ", plan.scope);
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
                    println!("kept");
                    return Ok(1);
                }
            }
            let n = c
                .store
                .forget(plan.start, plan.end, plan.app.as_deref(), plan.everything)?;
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
                let state = browser_connection_line(
                    s.installed,
                    s.program_ok,
                    &s.program,
                    s.extensions.len(),
                );
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

const ANCHOR_CONFIG: &str = "troubleshooting-config";
const ANCHOR_PRIVACY: &str = "troubleshooting-privacy-patterns";
const ANCHOR_HYPRLAND: &str = "troubleshooting-hyprland";
const ANCHOR_BUSCTL: &str = "troubleshooting-busctl";
const ANCHOR_BROWSER: &str = "troubleshooting-browser";
const ANCHOR_MODEL: &str = "troubleshooting-no-local-model";
const ANCHOR_CONSENT: &str = "troubleshooting-consent";
const ANCHOR_DAEMON: &str = "troubleshooting-daemon";
const ANCHOR_DATABASE: &str = "troubleshooting-database-permissions";

const FAIL_HYPRLAND: &str = "no running Hyprland instance";
const FAIL_BUSCTL: &str = "missing";
const FAIL_BROWSER: &str = "optional: run `omacortex browser install`";
const FAIL_CONSENT: &str = "run `omacortex enable`";
const FAIL_DAEMON: &str = "not running";

/// Failure details name the README anchor for that check. Passing checks stay as they are.
fn doctor_detail(ok: bool, detail: impl Into<String>, anchor: &str) -> String {
    let detail = detail.into();
    if ok {
        detail
    } else {
        format!("{detail} (README#{anchor})")
    }
}

fn doctor(as_json: bool) -> Res<i32> {
    use std::os::unix::fs::PermissionsExt;
    let cfg = Config::load();
    let mut checks: Vec<(String, bool, String)> = Vec::new();
    let mut add = |name: &str, ok: bool, detail: String, anchor: &str| {
        checks.push((name.into(), ok, doctor_detail(ok, detail, anchor)));
    };
    add(
        "config",
        cfg.error.is_empty(),
        if cfg.error.is_empty() {
            cfg.paths.config_file().display().to_string()
        } else {
            cfg.error.clone()
        },
        ANCHOR_CONFIG,
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
        ANCHOR_PRIVACY,
    );
    let dir = collectors::hyprland::socket_dir();
    add(
        "hyprland socket",
        dir.is_some(),
        if dir.is_some() {
            "found".into()
        } else {
            FAIL_HYPRLAND.into()
        },
        ANCHOR_HYPRLAND,
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
            FAIL_BUSCTL.into()
        },
        ANCHOR_BUSCTL,
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
            FAIL_BROWSER.into()
        } else {
            connected.join(", ")
        },
        ANCHOR_BROWSER,
    );
    match llm::resolve(&cfg.settings.model) {
        Ok(m) => add(
            "local model",
            true,
            format!("{} at {}", m.model, m.endpoint),
            ANCHOR_MODEL,
        ),
        Err(e) => add("local model", false, format!("optional: {e}"), ANCHOR_MODEL),
    }
    let c = Cortex::open(cfg, timeutil::system_clock())?;
    let s = c.status()?;
    add(
        "consent",
        s["consent"] == true,
        if s["consent"] == true {
            "enabled".into()
        } else {
            FAIL_CONSENT.into()
        },
        ANCHOR_CONSENT,
    );
    add(
        "daemon",
        s["daemon"] == true,
        if s["daemon"] == true {
            "running".into()
        } else {
            FAIL_DAEMON.into()
        },
        ANCHOR_DAEMON,
    );
    let mode = std::fs::metadata(c.cfg.paths.db())?.permissions().mode() & 0o777;
    add(
        "database permissions",
        mode == 0o600,
        format!("{} {mode:o}", c.cfg.paths.db().display()),
        ANCHOR_DATABASE,
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

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: f64 = 1_800_000_000.0;

    fn row(kind: &str, app: &str, title: &str, seconds: f64, ongoing: bool) -> Row {
        Row {
            id: 1,
            kind: kind.into(),
            source: "test".into(),
            app: app.into(),
            title: title.into(),
            detail: String::new(),
            started_at: NOW,
            ended_at: NOW,
            ongoing,
            seconds,
        }
    }

    fn item(kind: &str) -> Item {
        Item {
            kind: kind.into(),
            title: "Rates".into(),
            app: "chromium".into(),
            site: String::new(),
            creator: String::new(),
            location: String::new(),
            content: String::new(),
            first_seen: NOW,
            last_seen: NOW,
            seconds: 90.0,
            times: 1,
            last_id: 7,
        }
    }

    #[test]
    fn kind_names_are_the_stored_labels() {
        assert_eq!(
            kinds(&[
                Kind::Focus,
                Kind::Media,
                Kind::Page,
                Kind::File,
                Kind::Note,
                Kind::Marker
            ]),
            vec!["focus", "media", "page", "file", "note", "marker"]
        );
        assert!(kinds(&[]).is_empty());
    }

    #[test]
    fn status_line_prefers_consent_then_pause_then_away() {
        assert_eq!(
            recording_status_line(false, true, Some(NOW), true),
            "off: not enabled yet (run `omacortex enable`)"
        );
        assert_eq!(recording_status_line(true, true, None, true), "paused");
        assert_eq!(
            recording_status_line(true, true, Some(NOW), false),
            format!("paused until {}", clock(NOW))
        );
        assert_eq!(
            recording_status_line(true, false, None, true),
            "recording (you are away)"
        );
        assert_eq!(recording_status_line(true, false, None, false), "recording");
    }

    #[test]
    fn sensor_and_retention_phrases() {
        assert_eq!(enabled_sensors(false, false, false, false), "none");
        assert_eq!(
            enabled_sensors(true, false, true, true),
            "windows, files, browser"
        );
        assert_eq!(
            enabled_sensors(true, true, true, true),
            "windows, media, files, browser"
        );
        assert_eq!(retention_phrase(None), "forever");
        assert_eq!(retention_phrase(Some(0)), "forever");
        assert_eq!(retention_phrase(Some(30)), "30 days");
    }

    #[test]
    fn row_lines_drop_duration_for_point_events() {
        let stamp = timeutil::stamp(NOW);
        let focus = row("focus", "editor", "notes", 90.0, true);
        assert_eq!(
            row_line(&focus),
            format!("{stamp}     1m  [focus] editor: notes (now)")
        );
        let file = row("file", "gedit", "a.txt", 10.0, false);
        assert_eq!(row_line(&file), format!("{stamp}  [file] gedit: a.txt"));
        let note = row("note", "", "left off here", 0.0, false);
        assert_eq!(row_line(&note), format!("{stamp}  [note] left off here"));
        let marker = row("marker", "", "away", 5.0, false);
        assert_eq!(row_line(&marker), format!("{stamp}  [marker] away"));
    }

    #[test]
    fn item_lines_fold_repeats_source_and_location() {
        let stamp = timeutil::stamp(NOW);
        let mut page = item("page");
        page.content = "article".into();
        page.creator = "Ada".into();
        page.site = "example.com".into();
        page.location = "https://example.com/rates".into();
        page.seconds = 2580.0;
        page.times = 2;
        page.last_id = 42;
        assert_eq!(
            item_lines(&page),
            format!(
                "{stamp}  article Rates — Ada · example.com  (2×, 43m)\n                    https://example.com/rates  [id 42]"
            )
        );

        let once = item("media");
        assert_eq!(
            item_lines(&once),
            format!("{stamp}  media   Rates — chromium  (1m)")
        );

        let mut file = item("file");
        file.title = "notes.md".into();
        file.app.clear();
        file.seconds = 0.0;
        assert_eq!(item_lines(&file), format!("{stamp}  file    notes.md"));

        let mut noted = item("note");
        noted.times = 3;
        noted.app.clear();
        assert_eq!(item_lines(&noted), format!("{stamp}  note    Rates  (3×)"));
    }

    #[test]
    fn forget_plan_covers_each_way_to_choose_rows() {
        match plan_forget(NOW, Some("15m"), None, None, Some("signal".into()), true).unwrap() {
            ForgetChoice::Ready(plan) => {
                assert_eq!(plan.scope, "everything omaCortex has recorded");
                assert!(plan.everything);
                assert_eq!(plan.start, None);
                assert_eq!(plan.end, None);
                assert_eq!(plan.app, None);
            }
            ForgetChoice::MissingTarget => panic!("--all is a target"),
        }

        match plan_forget(NOW, Some("15m"), None, None, Some("signal".into()), false).unwrap() {
            ForgetChoice::Ready(plan) => {
                assert_eq!(plan.scope, "the last 15m");
                assert_eq!(plan.start, Some(NOW - 900.0));
                assert_eq!(plan.end, None);
                assert_eq!(plan.app, None);
                assert!(!plan.everything);
            }
            ForgetChoice::MissingTarget => panic!("--last is a target"),
        }

        match plan_forget(
            NOW,
            None,
            Some("2h"),
            Some("15m"),
            Some("signal".into()),
            false,
        )
        .unwrap()
        {
            ForgetChoice::Ready(plan) => {
                assert_eq!(plan.scope, "matching activity");
                assert_eq!(plan.start, Some(NOW - 7200.0));
                assert_eq!(plan.end, Some(NOW - 900.0));
                assert_eq!(plan.app.as_deref(), Some("signal"));
                assert!(!plan.everything);
            }
            ForgetChoice::MissingTarget => panic!("filters are a target"),
        }

        match plan_forget(NOW, None, None, None, Some("signal".into()), false).unwrap() {
            ForgetChoice::Ready(plan) => assert_eq!(plan.app.as_deref(), Some("signal")),
            ForgetChoice::MissingTarget => panic!("--app is a target"),
        }

        assert!(matches!(
            plan_forget(NOW, None, None, None, None, false).unwrap(),
            ForgetChoice::MissingTarget
        ));
        assert!(plan_forget(NOW, Some("nope"), None, None, None, false).is_err());
        assert!(plan_forget(NOW, None, Some("not-a-time"), None, None, false).is_err());
    }

    #[test]
    fn browser_connection_line_names_each_state() {
        assert_eq!(
            browser_connection_line(false, false, "/bin/omacortex", 0),
            "not connected (run `omacortex browser install`)"
        );
        assert_eq!(
            browser_connection_line(true, false, "/opt/omacortex", 2),
            "connected, but /opt/omacortex is missing; reinstall"
        );
        assert_eq!(
            browser_connection_line(true, true, "/opt/omacortex", 2),
            "connected for 2 extension(s)"
        );
    }

    fn readme_section<'a>(readme: &'a str, anchor: &str) -> &'a str {
        let marker = format!("id=\"{anchor}\"");
        let start = readme
            .find(&marker)
            .unwrap_or_else(|| panic!("README has no id=\"{anchor}\""));
        let rest = &readme[start + marker.len()..];
        let end = rest.find("<a id=\"").unwrap_or(rest.len());
        &rest[..end]
    }

    #[test]
    fn doctor_failure_strings_have_readme_anchors() {
        let readme = include_str!("../README.md");
        let static_failures = [
            (FAIL_HYPRLAND, ANCHOR_HYPRLAND),
            (FAIL_BUSCTL, ANCHOR_BUSCTL),
            (FAIL_BROWSER, ANCHOR_BROWSER),
            (FAIL_CONSENT, ANCHOR_CONSENT),
            (FAIL_DAEMON, ANCHOR_DAEMON),
        ];
        for (detail, anchor) in static_failures {
            let msg = doctor_detail(false, detail, anchor);
            assert!(msg.ends_with(&format!("(README#{anchor})")), "{msg}");
            assert!(
                readme_section(readme, anchor).contains(detail),
                "README#{anchor} does not quote {detail}"
            );
        }
        let dynamic = [
            ("config.toml: invalid type", ANCHOR_CONFIG, "does not parse"),
            ("(?bad", ANCHOR_PRIVACY, "exclude_titles"),
            (
                "optional: no local model answered",
                ANCHOR_MODEL,
                "optional:",
            ),
            ("cortex.db 644", ANCHOR_DATABASE, "0600"),
        ];
        for (detail, anchor, documented) in dynamic {
            let msg = doctor_detail(false, detail, anchor);
            assert!(msg.contains(&format!("README#{anchor}")), "{msg}");
            assert!(
                readme_section(readme, anchor).contains(documented),
                "README#{anchor} does not mention {documented}"
            );
        }
        assert_eq!(doctor_detail(true, "found", ANCHOR_HYPRLAND), "found");
        for anchor in [
            ANCHOR_CONFIG,
            ANCHOR_PRIVACY,
            ANCHOR_HYPRLAND,
            ANCHOR_BUSCTL,
            ANCHOR_BROWSER,
            ANCHOR_MODEL,
            ANCHOR_CONSENT,
            ANCHOR_DAEMON,
            ANCHOR_DATABASE,
        ] {
            assert!(
                readme.contains(&format!("id=\"{anchor}\"")),
                "README is missing {anchor}"
            );
        }
    }
}
