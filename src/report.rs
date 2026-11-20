//! Time windows (today, this week) and human-readable durations.

use chrono::{Datelike, Duration, Local, NaiveTime, TimeZone};

/// "2h 14m", "14m", "0m".
pub fn fmt_dur(secs: i64) -> String {
    let mins = secs.max(0) / 60;
    let (h, m) = (mins / 60, mins % 60);
    if h > 0 { format!("{h}h {m:02}m") } else { format!("{m}m") }
}

/// Unix time of today's local midnight.
pub fn start_of_today() -> i64 {
    let today = Local::now().date_naive();
    local_ts(today.and_time(NaiveTime::MIN))
}

/// Unix time of this week's Monday, local midnight.
pub fn start_of_week() -> i64 {
    let today = Local::now().date_naive();
    let monday = today - Duration::days(today.weekday().num_days_from_monday() as i64);
    local_ts(monday.and_time(NaiveTime::MIN))
}

fn local_ts(dt: chrono::NaiveDateTime) -> i64 {
    Local
        .from_local_datetime(&dt)
        .earliest()
        .map(|d| d.timestamp())
        .unwrap_or_else(|| dt.and_utc().timestamp())
}

pub fn now() -> i64 {
    Local::now().timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(fmt_dur(0), "0m");
        assert_eq!(fmt_dur(59), "0m");
        assert_eq!(fmt_dur(14 * 60), "14m");
        assert_eq!(fmt_dur(2 * 3600 + 14 * 60 + 30), "2h 14m");
        assert_eq!(fmt_dur(41 * 3600), "41h 00m");
    }

    #[test]
    fn week_starts_before_today() {
        assert!(start_of_week() <= start_of_today());
        assert!(start_of_today() <= now());
    }
}
