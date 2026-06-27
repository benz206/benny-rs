use std::time::Duration;

pub fn humanize_duration(d: Duration) -> String {
    let total = d.as_secs();
    let days = total / 86400;
    let hours = (total % 86400) / 3600;
    let mins = (total % 3600) / 60;
    let secs = total % 60;

    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if mins > 0 {
        parts.push(format!("{mins}m"));
    }
    if secs > 0 || parts.is_empty() {
        parts.push(format!("{secs}s"));
    }
    parts.join(" ")
}

pub fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        let end = s
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|&i| i <= max_len)
            .last()
            .unwrap_or(max_len);
        &s[..end]
    }
}

pub fn loading_bar(current: u64, total: u64, width: usize) -> String {
    if total == 0 {
        return "▓".repeat(width);
    }
    let filled = ((current as f64 / total as f64) * width as f64) as usize;
    let filled = filled.min(width);
    let empty = width - filled;
    format!("{}{}", "▓".repeat(filled), "░".repeat(empty))
}
