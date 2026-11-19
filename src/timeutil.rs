//! Time helpers shared by the CLI and the MCP server. Times are epoch seconds (f64).

use chrono::{DateTime, Duration, Local, NaiveDate, NaiveDateTime, TimeZone};
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
    Err(format!(
        "not a time: {raw:?} (try 'today', '2h', or 2026-11-19T09:00)"
    ))
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
}
