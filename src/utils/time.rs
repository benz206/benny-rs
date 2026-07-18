use chrono::{DateTime, Utc};
use interim::{Dialect, parse_date_string};

/// Parse a human time expression into an absolute UTC timestamp, relative to
/// `now`. Handles natural-language input ("tomorrow", "in 5 hours",
/// "next friday", "2026-04-01 09:00") via `interim`, falling back to a plain
/// duration ("1h30m", "45m") added to `now`.
///
/// Returns `None` if the input cannot be interpreted or resolves to the past.
pub fn parse_when(input: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }

    if let Ok(dt) = parse_date_string(input, now, Dialect::Us) {
        return Some(dt);
    }

    if let Some(dur) = super::parse::parse_duration(input)
        && let Ok(chrono_dur) = chrono::Duration::from_std(dur) {
            return now.checked_add_signed(chrono_dur);
        }

    None
}
