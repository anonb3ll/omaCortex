//! Turn raw focus spans into sessions a person would recognise.
//!
//! A session is a run of focus on one app. Short detours (under
//! `DETOUR_SECONDS`) to another app do not end it, and a gap shorter than
//! `MERGE_GAP` between two spans of the same app joins them.

use crate::store::Row;
use serde::Serialize;

const MERGE_GAP: f64 = 120.0;
const DETOUR_SECONDS: f64 = 30.0;

#[derive(Debug, Clone, Serialize)]
pub struct TitleTime {
    pub title: String,
    pub seconds: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Session {
    pub app: String,
    pub title: String,
    pub started_at: f64,
    pub ended_at: f64,
    pub active_seconds: f64,
    pub interruptions: u32,
    pub ongoing: bool,
    pub titles: Vec<TitleTime>,
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

fn extend(s: &mut Session, titles: &mut Vec<(String, f64)>, span: &Row) {
    s.ended_at = s.ended_at.max(span.ended_at);
    s.active_seconds += span.seconds;
    s.ongoing = span.ongoing;
    match titles.iter_mut().find(|(t, _)| *t == span.title) {
        Some((_, secs)) => *secs += span.seconds,
        None => titles.push((span.title.clone(), span.seconds)),
    }
}

pub fn build(spans: &[Row]) -> Vec<Session> {
    let mut focus: Vec<&Row> = spans.iter().filter(|r| r.kind == "focus").collect();
    focus.sort_by(|a, b| a.started_at.total_cmp(&b.started_at));
    let mut out: Vec<(Session, Vec<(String, f64)>)> = Vec::new();
    for span in focus {
        if let Some((last, titles)) = out.last_mut() {
            let gap = span.started_at - last.ended_at;
            if span.app == last.app && gap <= MERGE_GAP {
                extend(last, titles, span);
                continue;
            }
            if span.seconds < DETOUR_SECONDS && gap <= MERGE_GAP {
                // A brief detour: an interruption of the running session.
                last.interruptions += 1;
                last.ended_at = last.ended_at.max(span.ended_at);
                last.ongoing = span.ongoing;
                continue;
            }
        }
        let mut s = Session {
            app: span.app.clone(),
            title: String::new(),
            started_at: span.started_at,
            ended_at: span.ended_at,
            active_seconds: 0.0,
            interruptions: 0,
            ongoing: span.ongoing,
            titles: Vec::new(),
        };
        let mut titles = Vec::new();
        extend(&mut s, &mut titles, span);
        out.push((s, titles));
    }
    out.into_iter()
        .map(|(mut s, mut titles)| {
            titles.sort_by(|a, b| b.1.total_cmp(&a.1));
            s.titles = titles
                .into_iter()
                .filter(|(t, _)| !t.is_empty())
                .take(5)
                .map(|(title, secs)| TitleTime {
                    title,
                    seconds: round1(secs),
                })
                .collect();
            s.title = s
                .titles
                .first()
                .map(|t| t.title.clone())
                .unwrap_or_default();
            s.active_seconds = round1(s.active_seconds);
            s
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(app: &str, title: &str, start: f64, secs: f64, ongoing: bool) -> Row {
        Row {
            id: 0,
            kind: "focus".into(),
            source: "h".into(),
            app: app.into(),
            title: title.into(),
            detail: String::new(),
            started_at: start,
            ended_at: start + secs,
            ongoing,
            seconds: secs,
        }
    }

    #[test]
    fn merge_same_app_and_absorb_short_detours() {
        let s = build(&[
            span("kitty", "nvim a.py", 0.0, 600.0, false),
            span("chromium", "Docs", 600.0, 10.0, false),
            span("kitty", "nvim b.py", 610.0, 300.0, false),
            span("chromium", "Review PR", 1000.0, 900.0, false),
            span("chromium", "Docs", 1950.0, 100.0, true),
        ]);
        let got: Vec<_> = s
            .iter()
            .map(|x| (x.app.as_str(), x.active_seconds, x.interruptions))
            .collect();
        assert_eq!(got, vec![("kitty", 900.0, 1), ("chromium", 1000.0, 0)]);
        assert_eq!(s[0].title, "nvim a.py");
        assert!(s[1].ongoing);
    }

    #[test]
    fn long_gap_splits() {
        assert_eq!(
            build(&[
                span("k", "a", 0.0, 60.0, false),
                span("k", "a", 1000.0, 60.0, false)
            ])
            .len(),
            2
        );
    }
}
