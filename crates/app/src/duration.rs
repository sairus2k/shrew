//! How the app writes times and durations.

use chrono::{DateTime, Local, NaiveDateTime, TimeZone};

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

/// "1h 30m", "45m", "2h": hours and minutes in that order, each at most once.
/// Zero and anything else is refused with a message for the user.
pub fn parse_duration(text: &str) -> Result<u64, &'static str> {
    const INVALID: &str = "Enter the time like 1h 30m, 45m or 2h";
    let mut rest = text.trim();
    let mut seconds = 0u64;
    let mut unit_seen = 0;
    if rest.is_empty() {
        return Err(INVALID);
    }
    while !rest.is_empty() {
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let amount: u64 = rest[..digits].parse().map_err(|_| INVALID)?;
        let after = &rest[digits..];
        let (rank, unit_seconds) = match after.chars().next() {
            Some('h' | 'H') => (1, 3600),
            Some('m' | 'M') => (2, 60),
            _ => return Err(INVALID),
        };
        if rank <= unit_seen {
            return Err(INVALID);
        }
        unit_seen = rank;
        seconds = amount
            .checked_mul(unit_seconds)
            .and_then(|part| seconds.checked_add(part))
            .ok_or(INVALID)?;
        rest = after[1..].trim_start();
    }
    if seconds == 0 {
        return Err("The time must be more than zero");
    }
    Ok(seconds)
}

/// Unix seconds of "2026-03-29 10:15", read in the time zone of this computer:
/// the inverse of `local_minute`.
pub fn parse_local_minute(text: &str) -> Result<i64, &'static str> {
    parse_minute_in(text, &Local)
}

fn parse_minute_in<Z: TimeZone>(text: &str, zone: &Z) -> Result<i64, &'static str> {
    NaiveDateTime::parse_from_str(text.trim(), "%Y-%m-%d %H:%M")
        .ok()
        // A time that the clocks skipped on a change to summer time has no instant
        .and_then(|local| zone.from_local_datetime(&local).earliest())
        .map(|instant| instant.timestamp())
        .ok_or("Enter the start like 2026-03-29 10:15")
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

    #[test]
    fn a_duration_is_read_from_hours_and_minutes() {
        assert_eq!(parse_duration("1h 30m"), Ok(5400));
        assert_eq!(parse_duration("1h30m"), Ok(5400));
        assert_eq!(parse_duration("  2H "), Ok(7200));
        assert_eq!(parse_duration("45m"), Ok(2700));
        assert_eq!(parse_duration("90m"), Ok(5400));
        assert_eq!(parse_duration("0h 15m"), Ok(900));
    }

    #[test]
    fn a_duration_of_zero_is_refused_with_its_own_message() {
        let zero = Err("The time must be more than zero");
        assert_eq!(parse_duration("0m"), zero);
        assert_eq!(parse_duration("0h 0m"), zero);
    }

    #[test]
    fn a_duration_that_is_not_hours_and_minutes_is_refused() {
        let invalid = Err("Enter the time like 1h 30m, 45m or 2h");
        for text in [
            "",
            "  ",
            "abc",
            "90",
            "h",
            "1.5h",
            "-1h",
            "1d",
            "30m 1h",
            "1h 1h",
            "1h x",
            "1h 30",
            "99999999999999999999h",
            "9999999999999999h",
        ] {
            assert_eq!(parse_duration(text), invalid, "{text:?}");
        }
    }

    #[test]
    fn a_start_is_read_in_the_given_time_zone() {
        let paris = FixedOffset::east_opt(2 * 3600).unwrap();
        assert_eq!(
            parse_minute_in("2026-03-29 10:15", &paris),
            Ok(1_774_772_100)
        );
        assert_eq!(
            parse_minute_in(" 2026-03-29 08:15 ", &chrono::Utc),
            Ok(1_774_772_100)
        );
    }

    #[test]
    fn a_start_that_is_not_a_date_and_time_is_refused() {
        let invalid = Err("Enter the start like 2026-03-29 10:15");
        for text in [
            "",
            "yesterday",
            "2026-03-29",
            "2026-13-01 10:00",
            "2026-03-29 25:00",
        ] {
            assert_eq!(parse_minute_in(text, &chrono::Utc), invalid, "{text:?}");
        }
    }
}
