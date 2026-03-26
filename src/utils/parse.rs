use humantime::parse_duration as humantime_parse;
use std::time::Duration;

pub fn parse_duration(s: &str) -> Option<Duration> {
    humantime_parse(s).ok()
}
