use humantime::parse_duration as humantime_parse;
use std::time::Duration;

pub fn parse_duration(s: &str) -> Option<Duration> {
    humantime_parse(s).ok()
}

/// Extract a raw u64 snowflake from a mention like `<@123>`, `<@!123>`,
/// `<#123>`, `<@&123>`, or a bare id string. Returns None if no digits found.
pub fn parse_id(s: &str) -> Option<u64> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// Parse a channel id from `<#id>` / bare id.
pub fn parse_channel_id(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.starts_with("<#") && s.ends_with('>') {
        return parse_id(s);
    }
    parse_id(s).filter(|_| s.chars().all(|c| c.is_ascii_digit()))
}

/// Parse a role id from `<@&id>` / bare id.
pub fn parse_role_id(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.starts_with("<@&") && s.ends_with('>') {
        return parse_id(s);
    }
    parse_id(s).filter(|_| s.chars().all(|c| c.is_ascii_digit()))
}
