//! How the app writes times and durations.

use chrono::{DateTime, Local, TimeZone};

/// "1h 23m". A part that is zero is left out, whole minutes only, and a span
/// of a day or more stays in hours: a worklog is counted in working time.
pub fn format_duration(seconds: u64) -> String {
    let (hours, minutes) = (seconds / 3600, seconds / 60 % 60);
    match (hours, minutes) {
        (0, 0) => "<1m".to_owned(),
        (hours, 0) => format!("{hours}h"),
        (0, minutes) => format!("{minutes}m"),
        (hours, minutes) => format!("{hours}h {minutes}m"),
    }
}

/// "2026-03-29 10:15" for Unix seconds, in the time zone of this computer.
pub fn local_minute(seconds: i64) -> String {
    minute_in(seconds, &Local)
}

fn minute_in<Z: TimeZone>(seconds: i64, zone: &Z) -> String
where
    Z::Offset: std::fmt::Display,
{
    match DateTime::from_timestamp(seconds, 0) {
        Some(instant) => instant
            .with_timezone(zone)
            .format("%Y-%m-%d %H:%M")
            .to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    #[test]
    fn a_duration_is_written_in_hours_and_minutes() {
        assert_eq!(format_duration(4980), "1h 23m");
        assert_eq!(format_duration(3600 * 8 + 60), "8h 1m");
    }

    #[test]
    fn a_part_that_is_zero_is_left_out() {
        assert_eq!(format_duration(3600), "1h");
        assert_eq!(format_duration(5 * 3600), "5h");
        assert_eq!(format_duration(25 * 60), "25m");
    }

    #[test]
    fn seconds_are_dropped_and_less_than_a_minute_is_said_so() {
        assert_eq!(format_duration(4980 + 59), "1h 23m");
        assert_eq!(format_duration(59), "<1m");
        assert_eq!(format_duration(0), "<1m");
    }

    #[test]
    fn a_long_duration_stays_in_hours() {
        assert_eq!(format_duration(26 * 3600 + 30 * 60), "26h 30m");
    }

    #[test]
    fn the_start_is_shown_in_the_given_time_zone() {
        // 08:15 UTC
        let at = 1_774_772_100;
        assert_eq!(minute_in(at, &chrono::Utc), "2026-03-29 08:15");
        let paris = FixedOffset::east_opt(2 * 3600).unwrap();
        assert_eq!(minute_in(at, &paris), "2026-03-29 10:15");
    }
}
