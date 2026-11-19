//! Time helpers shared by the CLI and the MCP server. Times are epoch seconds (f64).

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, NaiveDateTime, TimeZone};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn system_clock() -> Clock {
    Arc::new(now)
}

fn local(ts: f64) -> DateTime<Local> {
    let secs = ts.floor() as i64;
    let nanos = ((ts - ts.floor()) * 1e9) as u32;
    Local
        .timestamp_opt(secs, nanos)
        .earliest()
        .unwrap_or_else(Local::now)
}

fn midnight(date: NaiveDate) -> f64 {
    let naive = date.and_hms_opt(0, 0, 0).expect("midnight exists");
    match Local.from_local_datetime(&naive).earliest() {
        Some(dt) => dt.timestamp() as f64,
        // A DST gap at midnight: take the first valid instant after it.
        None => Local
            .from_local_datetime(&(naive + Duration::hours(1)))
            .earliest()
            .map_or(0.0, |dt| dt.timestamp() as f64),
    }
}

pub fn start_of_day(ts: f64) -> f64 {
    midnight(local(ts).date_naive())
}

/// '90s', '15m', '2h', '1d', '1w'. A bare number means minutes.
pub fn parse_duration(text: &str) -> Result<f64, String> {
    let t = text.trim().to_lowercase();
    let (num, unit) = match t.char_indices().find(|(_, c)| c.is_ascii_alphabetic()) {
        Some((i, _)) => (t[..i].trim(), t[i..].trim()),
        None => (t.as_str(), ""),
    };
    let mult = match unit {
        "" | "m" => 60.0,
        "s" => 1.0,
        "h" => 3600.0,
        "d" => 86400.0,
        "w" => 604800.0,
        _ => return Err(format!("not a duration: {text:?} (try 15m, 2h, 1d)")),
    };
    match num.parse::<f64>() {
        Ok(n) if n >= 0.0 && n.is_finite() => Ok(n * mult),
        _ => Err(format!("not a duration: {text:?} (try 15m, 2h, 1d)")),
    }
}

/// Parse a point in time: 'now', 'today', 'yesterday', a duration meaning
/// "that long ago" ('2h'), epoch seconds, an ISO date or date-time.
pub fn parse_when(text: Option<&str>, now: f64) -> Result<Option<f64>, String> {
    let Some(raw) = text.map(str::trim).filter(|t| !t.is_empty()) else {
        return Ok(None);
    };
    let value = raw.to_lowercase();
    match value.as_str() {
        "now" => return Ok(Some(now)),
        "today" => return Ok(Some(start_of_day(now))),
        "yesterday" => return Ok(Some(midnight(local(now).date_naive() - Duration::days(1)))),
        _ => {}
    }
    if let Ok(d) = parse_duration(&value) {
        return Ok(Some(now - d));
    }
    if value.len() >= 9
        && value.chars().all(|c| c.is_ascii_digit() || c == '.')
        && let Ok(v) = value.parse::<f64>()
    {
        return Ok(Some(v));
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Ok(Some(dt.timestamp() as f64));
    }
    for fmt in [
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(n) = NaiveDateTime::parse_from_str(raw, fmt)
            && let Some(dt) = Local.from_local_datetime(&n).earliest()
        {
            return Ok(Some(dt.timestamp() as f64));
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return Ok(Some(midnight(d)));
    }
    if let Some((start, _)) = parse_range(&value, now) {
        return Ok(Some(start));
    }
    Err(format!(
        "not a time: {raw:?} (try 'today', '2h', 'last tuesday', or 2026-11-19T09:00)"
    ))
}

const WEEKDAYS: [(&str, &str); 7] = [
    ("monday", "mon"),
    ("tuesday", "tue"),
    ("wednesday", "wed"),
    ("thursday", "thu"),
    ("friday", "fri"),
    ("saturday", "sat"),
    ("sunday", "sun"),
];
const MONTHS: [(&str, &str); 12] = [
    ("january", "jan"),
    ("february", "feb"),
    ("march", "mar"),
    ("april", "apr"),
    ("may", "may"),
    ("june", "jun"),
    ("july", "jul"),
    ("august", "aug"),
    ("september", "sep"),
    ("october", "oct"),
    ("november", "nov"),
    ("december", "dec"),
];

fn index_of(names: &[(&str, &str)], word: &str, short: bool) -> Option<u32> {
    names
        .iter()
        .position(|(full, abbr)| *full == word || (short && *abbr == word))
        .map(|i| i as u32)
}

fn count(word: &str) -> Option<f64> {
    match word {
        "a" | "an" | "one" => Some(1.0),
        "two" => Some(2.0),
        "three" => Some(3.0),
        "few" | "couple" => Some(3.0),
        _ => word
            .parse::<f64>()
            .ok()
            .filter(|n| *n >= 0.0 && n.is_finite()),
    }
}

fn unit_seconds(word: &str) -> Option<f64> {
    match word.trim_end_matches('s') {
        "minute" | "min" => Some(60.0),
        "hour" | "hr" => Some(3600.0),
        "day" => Some(86400.0),
        "week" => Some(604800.0),
        "month" => Some(30.0 * 86400.0),
        _ => None,
    }
}

fn day_range(date: NaiveDate) -> (f64, f64) {
    (midnight(date), midnight(date + Duration::days(1)))
}

fn month_range(year: i32, month: u32) -> Option<(f64, f64)> {
    let first = NaiveDate::from_ymd_opt(year, month, 1)?;
    let next = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)?
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)?
    };
    Some((midnight(first), midnight(next)))
}

/// The stretch of time a phrase names, as (start, end): 'yesterday', 'last
/// tuesday', 'this morning', 'last week', '3 days ago', 'past 2 weeks',
/// 'october', or an ISO date. `short` also accepts 'tue' and 'oct'.
fn range_of(words: &[&str], now: f64, short: bool) -> Option<(f64, f64)> {
    let today = local(now).date_naive();
    let hours = |date: NaiveDate, from: i64, to: i64| {
        let base = midnight(date);
        (base + from as f64 * 3600.0, base + to as f64 * 3600.0)
    };
    let part = |date: NaiveDate, word: &str| match word {
        "morning" => Some(hours(date, 4, 12)),
        "afternoon" => Some(hours(date, 12, 18)),
        "evening" | "night" => Some(hours(date, 18, 28)),
        _ => None,
    };
    match words {
        ["today"] => Some(day_range(today)),
        ["yesterday"] => Some(day_range(today - Duration::days(1))),
        ["tonight"] => Some(hours(today, 18, 28)),
        ["this", word] if part(today, word).is_some() => part(today, word),
        ["last", "night"] => Some(hours(today - Duration::days(1), 18, 28)),
        ["yesterday", word] => part(today - Duration::days(1), word),
        ["this", "week"] | ["last", "week"] => {
            let monday = today - Duration::days(today.weekday().num_days_from_monday() as i64);
            let start = if words[0] == "last" {
                monday - Duration::days(7)
            } else {
                monday
            };
            Some((midnight(start), midnight(start + Duration::days(7))))
        }
        ["this", "month"] => month_range(today.year(), today.month()),
        ["last", "month"] => {
            if today.month() == 1 {
                month_range(today.year() - 1, 12)
            } else {
                month_range(today.year(), today.month() - 1)
            }
        }
        // "past 3 days", "last 2 weeks", "past hour": up to now.
        ["past" | "last", n, unit] => Some((now - count(n)? * unit_seconds(unit)?, now)),
        ["past", unit] => Some((now - unit_seconds(unit)?, now)),
        // "3 days ago": that day; hours and minutes: roughly then.
        [n, unit, "ago"] => {
            let (n, secs) = (count(n)?, unit_seconds(unit)?);
            if secs >= 604800.0 {
                let day = local(now - n * secs).date_naive();
                Some((
                    midnight(day - Duration::days(3)),
                    midnight(day + Duration::days(4)),
                ))
            } else if secs >= 86400.0 {
                Some(day_range(local(now - n * secs).date_naive()))
            } else {
                Some((now - n * secs * 1.5, now - n * secs * 0.5))
            }
        }
        ["last", day] | ["on", day] if index_of(&WEEKDAYS, day, short).is_some() => {
            let want = index_of(&WEEKDAYS, day, short)?;
            let mut back = (today.weekday().num_days_from_monday() + 7 - want) % 7;
            if back == 0 && words[0] == "last" {
                back = 7;
            }
            Some(day_range(today - Duration::days(back as i64)))
        }
        ["in" | "last", month] if index_of(&MONTHS, month, short).is_some() => {
            let m = index_of(&MONTHS, month, short)? + 1;
            let year = if m <= today.month() {
                today.year()
            } else {
                today.year() - 1
            };
            month_range(year, m)
        }
        [word] => {
            if let Ok(d) = NaiveDate::parse_from_str(word, "%Y-%m-%d") {
                return Some(day_range(d));
            }
            if let Some(want) = index_of(&WEEKDAYS, word, short) {
                let back = (today.weekday().num_days_from_monday() + 7 - want) % 7;
                return Some(day_range(today - Duration::days(back as i64)));
            }
            // "may" and "march" are ordinary words; they need "in" before them.
            let m = index_of(&MONTHS, word, short).filter(|m| short || ![2, 4].contains(m))? + 1;
            let year = if m <= today.month() {
                today.year()
            } else {
                today.year() - 1
            };
            month_range(year, m)
        }
        _ => None,
    }
}

pub fn parse_range(text: &str, now: f64) -> Option<(f64, f64)> {
    let lower = text.trim().to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    range_of(&words, now, true)
}

/// Find the first time phrase in free text. Returns its range and the words
/// left over, so "the talk I watched last tuesday" searches for the talk
/// inside that day. Abbreviations are not taken here: "sat" is also a verb.
pub fn extract_range(text: &str, now: f64) -> (Option<(f64, f64)>, Vec<String>) {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !(c.is_alphanumeric() || c == '-'))
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect();
    for start in 0..words.len() {
        for len in (1..=3.min(words.len() - start)).rev() {
            let slice: Vec<&str> = words[start..start + len]
                .iter()
                .map(String::as_str)
                .collect();
            if let Some(range) = range_of(&slice, now, false) {
                let mut rest = words[..start].to_vec();
                // A leading "on", "in", "from", "since" or "during" belongs to the phrase.
                if rest.last().is_some_and(|w| {
                    matches!(w.as_str(), "on" | "in" | "from" | "since" | "during")
                }) {
                    rest.pop();
                }
                rest.extend_from_slice(&words[start + len..]);
                return (Some(range), rest);
            }
        }
    }
    (None, words)
}

/// (YYYY-MM-DD, start, end) for 'today', 'yesterday' or an ISO date.
pub fn day_bounds(day: &str, now: f64) -> Result<(String, f64, f64), String> {
    let date = match day.trim() {
        "" | "today" => local(now).date_naive(),
        "yesterday" => local(now).date_naive() - Duration::days(1),
        other => NaiveDate::parse_from_str(other, "%Y-%m-%d")
            .map_err(|_| format!("bad day: {other:?}"))?,
    };
    Ok((
        date.format("%Y-%m-%d").to_string(),
        midnight(date),
        midnight(date + Duration::days(1)),
    ))
}

pub fn human_duration(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    if s < 60 {
        return format!("{s}s");
    }
    let m = s / 60;
    if m < 60 {
        return format!("{m}m");
    }
    let (h, m) = (m / 60, m % 60);
    if m == 0 {
        format!("{h}h")
    } else {
        format!("{h}h{m:02}m")
    }
}

pub fn clock(ts: f64) -> String {
    local(ts).format("%H:%M").to_string()
}

pub fn stamp(ts: f64) -> String {
    local(ts).format("%m-%d %H:%M").to_string()
}

pub fn iso(ts: f64) -> String {
    local(ts).format("%Y-%m-%dT%H:%M:%S%:z").to_string()
}

pub fn iso_opt(ts: Option<f64>) -> Option<String> {
    ts.map(iso)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_time() {
        let now = 1_800_000_000.0;
        assert_eq!(parse_when(Some("2h"), now).unwrap(), Some(now - 7200.0));
        assert_eq!(parse_when(Some("15m"), now).unwrap(), Some(now - 900.0));
        assert_eq!(parse_when(Some("now"), now).unwrap(), Some(now));
        assert_eq!(parse_when(None, now).unwrap(), None);
        assert!(parse_when(Some("today"), now).unwrap().unwrap() <= now);
        assert!(parse_when(Some("2026-11-19"), now).unwrap().unwrap() > 0.0);
        assert!(parse_when(Some("2026-11-19T09:00"), now).unwrap().is_some());
        assert!(parse_when(Some("next tuesday"), now).is_err());
        assert_eq!(parse_duration("90s").unwrap(), 90.0);
        assert_eq!(parse_duration("10").unwrap(), 600.0);
        assert!(parse_duration("-5m").is_err());
        assert_eq!(human_duration(59.0), "59s");
        assert_eq!(human_duration(3660.0), "1h01m");
        let (label, start, end) = day_bounds("2026-11-19", now).unwrap();
        assert_eq!(label, "2026-11-19");
        assert!((82800.0..=90000.0).contains(&(end - start)));
    }

    /// Noon on a known local date, whatever the test machine's time zone.
    fn noon(y: i32, m: u32, d: u32) -> f64 {
        midnight(NaiveDate::from_ymd_opt(y, m, d).unwrap()) + 12.0 * 3600.0
    }

    fn day(y: i32, m: u32, d: u32) -> (f64, f64) {
        day_range(NaiveDate::from_ymd_opt(y, m, d).unwrap())
    }

    #[test]
    fn phrases_name_ranges() {
        // 2026-11-19 is a Thursday.
        let now = noon(2026, 11, 19);
        assert_eq!(parse_range("yesterday", now), Some(day(2026, 11, 18)));
        assert_eq!(parse_range("tuesday", now), Some(day(2026, 11, 17)));
        assert_eq!(parse_range("last tuesday", now), Some(day(2026, 11, 17)));
        assert_eq!(parse_range("thursday", now), Some(day(2026, 11, 19)));
        assert_eq!(parse_range("last thursday", now), Some(day(2026, 11, 12)));
        assert_eq!(parse_range("on fri", now), Some(day(2026, 11, 13)));
        assert_eq!(parse_range("3 days ago", now), Some(day(2026, 11, 16)));
        assert_eq!(parse_range("2026-11-02", now), Some(day(2026, 11, 2)));
        let (start, end) = parse_range("last week", now).unwrap();
        assert_eq!((start, end), (day(2026, 11, 9).0, day(2026, 11, 15).1));
        assert_eq!(
            parse_range("this week", now).unwrap().0,
            day(2026, 11, 16).0
        );
        assert_eq!(
            parse_range("last month", now).unwrap(),
            (day(2026, 10, 1).0, day(2026, 11, 1).0)
        );
        assert_eq!(
            parse_range("in december", now).unwrap().0,
            day(2025, 12, 1).0
        );
        assert_eq!(parse_range("past 2 days", now), Some((now - 172800.0, now)));
        assert_eq!(
            parse_range("an hour ago", now),
            Some((now - 5400.0, now - 1800.0))
        );
        let (m0, m1) = parse_range("this morning", now).unwrap();
        assert_eq!(
            (m1 - m0, m0 - day(2026, 11, 19).0),
            (8.0 * 3600.0, 4.0 * 3600.0)
        );
        assert_eq!(parse_range("next tuesday", now), None);
        assert_eq!(parse_range("whenever", now), None);
        assert_eq!(
            parse_when(Some("last tuesday"), now).unwrap(),
            Some(day(2026, 11, 17).0)
        );
    }

    #[test]
    fn time_phrases_come_out_of_questions() {
        let now = noon(2026, 11, 19);
        let (range, rest) = extract_range("the Rust talk I watched last Tuesday?", now);
        assert_eq!(range, Some(day(2026, 11, 17)));
        assert_eq!(rest, ["the", "rust", "talk", "i", "watched"]);
        let (range, rest) = extract_range("substack article on monday about rates", now);
        assert_eq!(range, Some(day(2026, 11, 16)));
        assert_eq!(rest, ["substack", "article", "about", "rates"]);
        // Ordinary words that are also abbreviations or month names stay put.
        let (range, rest) = extract_range("the cat sat on the mat and may march off", now);
        assert_eq!(range, None);
        assert_eq!(rest.len(), 10);
        assert_eq!(
            extract_range("budget in october", now).0.unwrap().0,
            day(2026, 10, 1).0
        );
    }
}
