use super::{TagOutput, context::TagContext};
use rand::RngExt;
use serde_json::{Map, Value};

/// Dispatch a single block. `parameter` and `payload` are already resolved
/// (nested blocks collapsed). Returns the block's text contribution.
pub fn process_block(
    name: &str,
    parameter: Option<&str>,
    payload: Option<&str>,
    ctx: &mut TagContext,
    output: &mut TagOutput,
) -> String {
    let lname = name.to_lowercase();

    // Loose variable assignment: {$name:value}
    if let Some(var) = name.strip_prefix('$') {
        if !var.is_empty() {
            ctx.vars
                .insert(var.to_string(), payload.unwrap_or("").to_string());
        }
        return String::new();
    }

    match lname.as_str() {
        // ---- Assignment / variables --------------------------------------
        // {=(name):value} / {let(name):value} / {var(name):value}
        "=" | "let" | "var" => {
            if let Some(var) = parameter
                && !var.is_empty() {
                    ctx.vars
                        .insert(var.to_string(), payload.unwrap_or("").to_string());
                }
            String::new()
        }

        // ---- Comments -----------------------------------------------------
        // {//:...} {/...} {comment:...}. ({#...} handled in the default arm.)
        "//" | "/" | "comment" => String::new(),

        // ---- Conditionals -------------------------------------------------
        // {if(left==right):a|b}
        "if" => {
            let result = parse_if(parameter.unwrap_or(""));
            parse_into_output(payload.unwrap_or(""), result)
        }
        // {all(a|b|c):yes|no} / {and(...)}
        "all" | "and" => {
            let result = parse_list_all(parameter.unwrap_or(""), true);
            parse_into_output(payload.unwrap_or(""), result)
        }
        // {any(a|b|c):yes|no} / {or(...)}
        "any" | "or" => {
            let result = parse_list_all(parameter.unwrap_or(""), false);
            parse_into_output(payload.unwrap_or(""), result)
        }
        // {not:bool}
        "not" => match parse_if(payload.unwrap_or("")) {
            Some(b) => (!b).to_string(),
            None => String::new(),
        },

        // ---- Break / stop -------------------------------------------------
        // {break(cond):msg} — override final output (processing continues)
        "break" | "short" | "shortcircuit" => {
            if parse_if(parameter.unwrap_or("")).unwrap_or(false) {
                ctx.break_body = Some(payload.unwrap_or("").to_string());
            }
            String::new()
        }
        // {stop(cond):msg} / {halt} / {error} — halt all processing
        "stop" | "halt" => {
            if parse_if(parameter.unwrap_or("")).unwrap_or(false) {
                output.stopped = true;
                output.content = payload.unwrap_or("").to_string();
            }
            String::new()
        }

        // ---- Randomness ---------------------------------------------------
        // {choose:a|b|c}
        "choose" => {
            let choices = split_pipe(payload.unwrap_or(""));
            pick(&choices, parameter).trim().to_string()
        }
        // {random:a~b~c} / {rand:...} (weights via `weight|item`)
        "random" | "rand" => random_block(parameter, payload.unwrap_or("")),
        // {range(min,max)} / {range:max} / {range:min-max} / {rangef:..}
        "range" | "rangef" => range_block(parameter, payload.unwrap_or(""), lname == "rangef"),

        // ---- Math ---------------------------------------------------------
        // {math:expr} / {m:expr} / {calc:expr} / {+:expr}
        "math" | "m" | "calc" | "+" => match eval_math(payload.unwrap_or("")) {
            Some(n) => format_num(n),
            None => format!("<{} error>", name),
        },
        // {ord(c|i):n}
        "ord" => ordinal(parameter, payload.unwrap_or(""), name),

        // ---- Text ---------------------------------------------------------
        // {upper:text}
        "upper" => payload.unwrap_or("").to_uppercase(),
        // {lower:text}
        "lower" => payload.unwrap_or("").to_lowercase(),
        // {len(w|s):text} / {length:text}
        "len" | "length" => length_block(parameter, payload.unwrap_or("")),
        // {count(needle):haystack}
        "count" => match parameter {
            Some(needle) if !needle.is_empty() => {
                payload.unwrap_or("").matches(needle).count().to_string()
            }
            _ => (payload.unwrap_or("").chars().count() + 1).to_string(),
        },
        // {replace(from,to):text} / {sub(from,to):text}
        "replace" | "sub" => match parameter.and_then(|p| p.split_once(',')) {
            Some((from, to)) => payload.unwrap_or("").replace(from, to),
            None => payload.unwrap_or("").to_string(),
        },
        // {urlencode(+):text}
        "urlencode" => url_encode(payload.unwrap_or(""), parameter == Some("+")),
        // {urldecode(+):text}
        "urldecode" => url_decode(payload.unwrap_or(""), parameter == Some("+")),

        // ---- Time ---------------------------------------------------------
        // {unix} — current unix timestamp
        "unix" => chrono::Utc::now().timestamp().to_string(),
        // {strf(timestamp):format}  (bTagScript order)
        "strf" => strftime(parameter, payload, false),
        // {strftime(format):timestamp}  (spec order)
        "strftime" => strftime(parameter, payload, true),

        // ---- Discord side effects ----------------------------------------
        // {embed(field):value} / {embed({json})}
        "embed" => {
            embed_block(parameter, payload, output);
            String::new()
        }
        // {react:emoji}
        "react" => {
            let body = payload.unwrap_or("");
            if !body.is_empty() {
                let sep = if body.contains('~') { '~' } else { ',' };
                for e in body.split(sep).take(5) {
                    let e = e.trim();
                    if !e.is_empty() {
                        output.react_emojis.push(e.to_string());
                    }
                }
            }
            String::new()
        }
        // {delete} / {delete(cond)}
        "delete" | "del" => {
            let do_delete = match parameter {
                None => true,
                Some(cond) => parse_if(cond).unwrap_or(false),
            };
            if do_delete {
                output.delete_invoke = true;
            }
            String::new()
        }
        // {redirect:channel} / {redirect(channel)}
        "redirect" => {
            let target = parameter.or(payload).unwrap_or("");
            if let Some(id) = extract_channel_id(target) {
                output.redirect_channel = Some(id);
            }
            String::new()
        }
        // {cd(seconds):key} / {cooldown(seconds):key}
        "cd" | "cooldown" => {
            cooldown_block(parameter, payload, output);
            String::new()
        }

        // ---- Debug / no-ops ----------------------------------------------
        // {debug} — engine has no debug surface; render nothing.
        "debug" => String::new(),

        // ---- SECURITY: omitted (never execute arbitrary code/commands) ---
        "python" | "py" | "command" | "c" | "com" | "require" | "whitelist" | "blacklist"
        | "override" | "bypass" => String::new(),

        // ---- Variables & unknown blocks ----------------------------------
        _ => {
            // {#:list} is bTagScript's random alias; {#comment} is a comment.
            if name == "#" && payload.is_some() {
                return random_block(parameter, payload.unwrap_or(""));
            }
            if name.starts_with('#') {
                return String::new();
            }
            // Digit shorthand: {1} == {args(1)}
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_digit())
                && let Some(v) = ctx.get_var("args", Some(name), payload) {
                    return v;
                }
            // Built-in / user variable lookup.
            if let Some(v) = ctx.get_var(name, parameter, payload) {
                return v;
            }
            // Unknown: leave the block verbatim (matches bTagScript).
            reconstruct(name, parameter, payload)
        }
    }
}

// ---------------------------------------------------------------------------
// Boolean expression parsing
// ---------------------------------------------------------------------------

/// Evaluate an expression like `a==b`, `5>=3`, `true`. `None` = unparseable.
fn parse_if(s: &str) -> Option<bool> {
    match s.trim().to_lowercase().as_str() {
        "true" => return Some(true),
        "false" => return Some(false),
        _ => {}
    }
    for op in ["!=", "==", ">=", "<=", ">", "<"] {
        if let Some((l, r)) = s.split_once(op) {
            let (l, r) = (l.trim(), r.trim());
            return Some(match op {
                "==" => l == r,
                "!=" => l != r,
                _ => {
                    let lf: f64 = l.parse().ok()?;
                    let rf: f64 = r.parse().ok()?;
                    match op {
                        ">=" => lf >= rf,
                        "<=" => lf <= rf,
                        ">" => lf > rf,
                        "<" => lf < rf,
                        _ => unreachable!(),
                    }
                }
            });
        }
    }
    None
}

/// `{all}` (require_all=true) / `{any}` (require_all=false) over `a|b|c`.
fn parse_list_all(s: &str, require_all: bool) -> Option<bool> {
    let items = split_pipe(s);
    let results: Vec<bool> = items.iter().map(|i| parse_if(i).unwrap_or(false)).collect();
    if results.is_empty() {
        return Some(false);
    }
    Some(if require_all {
        results.iter().all(|&b| b)
    } else {
        results.iter().any(|&b| b)
    })
}

/// Pick the true/false branch of `a|b` based on the condition result.
fn parse_into_output(payload: &str, result: Option<bool>) -> String {
    let parts = split_pipe(payload);
    match result {
        Some(true) => {
            if parts.len() == 2 {
                parts[0].clone()
            } else {
                payload.to_string()
            }
        }
        Some(false) | None => {
            if parts.len() == 2 {
                parts[1].clone()
            } else {
                String::new()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Splitting helpers
// ---------------------------------------------------------------------------

/// Split on `|` that isn't backslash-escaped, then unescape `\|` -> `|`.
fn split_pipe(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&next) = chars.peek()
                && next == '|' {
                    cur.push('|');
                    chars.next();
                    continue;
                }
            cur.push('\\');
        } else if c == '|' {
            parts.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    parts.push(cur);
    parts
}

// ---------------------------------------------------------------------------
// Random / range
// ---------------------------------------------------------------------------

/// Deterministic index from a seed string (for seeded random/range).
fn seed_index(seed: &str, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let mut h: u64 = 1469598103934665603;
    for b in seed.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    (h % len as u64) as usize
}

/// Choose from `choices`; if `seed` is set the choice is deterministic.
fn pick<'a>(choices: &'a [String], seed: Option<&str>) -> &'a str {
    if choices.is_empty() {
        return "";
    }
    let idx = match seed.filter(|s| !s.is_empty()) {
        Some(s) => seed_index(s, choices.len()),
        None => rand::rng().random_range(0..choices.len()),
    };
    &choices[idx]
}

/// {random:a~b~c} — split on `~` (or `,`), honoring `weight|item` weights.
fn random_block(seed: Option<&str>, payload: &str) -> String {
    let sep = if payload.contains('~') { '~' } else { ',' };
    let mut items = Vec::new();
    let mut weights = Vec::new();
    for raw in payload.split(sep) {
        if let Some((w, item)) = raw.split_once('|') {
            weights.push(w.trim().parse::<u32>().unwrap_or(1).max(1));
            items.push(item.to_string());
        } else {
            weights.push(1);
            items.push(raw.to_string());
        }
    }
    if items.is_empty() {
        return String::new();
    }
    let total: u32 = weights.iter().sum();
    let roll = match seed.filter(|s| !s.is_empty()) {
        Some(s) => (seed_index(s, total as usize)) as u32,
        None => rand::rng().random_range(0..total),
    };
    let mut acc = 0u32;
    for (i, w) in weights.iter().enumerate() {
        acc += w;
        if roll < acc {
            return items[i].clone();
        }
    }
    items[items.len() - 1].clone()
}

/// {range(min,max)} / {range:max} / {range:min-max}; `floaty` => one decimal.
fn range_block(parameter: Option<&str>, payload: &str, floaty: bool) -> String {
    let mut seed: Option<&str> = None;
    let (min, max): (f64, f64) = if let Some(p) = parameter.filter(|p| p.contains(',')) {
        let (a, b) = p.split_once(',').unwrap();
        (
            a.trim().parse().unwrap_or(0.0),
            b.trim().parse().unwrap_or(0.0),
        )
    } else {
        seed = parameter.filter(|s| !s.is_empty());
        let p = payload.trim();
        // `min-max` (allow a leading negative min).
        if let Some(idx) = p
            .char_indices()
            .skip(1)
            .find(|&(_, c)| c == '-')
            .map(|(i, _)| i)
        {
            let (a, b) = p.split_at(idx);
            (
                a.trim().parse().unwrap_or(0.0),
                b[1..].trim().parse().unwrap_or(0.0),
            )
        } else {
            (0.0, p.parse().unwrap_or(0.0))
        }
    };

    let (lo, hi) = if min <= max { (min, max) } else { (max, min) };
    if floaty {
        let lo10 = (lo * 10.0) as i64;
        let hi10 = (hi * 10.0) as i64;
        let n = pick_int(lo10, hi10, seed);
        format_num(n as f64 / 10.0)
    } else {
        let n = pick_int(lo as i64, hi as i64, seed);
        n.to_string()
    }
}

fn pick_int(lo: i64, hi: i64, seed: Option<&str>) -> i64 {
    if lo >= hi {
        return lo;
    }
    let span = (hi - lo + 1) as usize;
    match seed {
        Some(s) => lo + seed_index(s, span) as i64,
        None => rand::rng().random_range(lo..=hi),
    }
}

// ---------------------------------------------------------------------------
// Math
// ---------------------------------------------------------------------------

fn eval_math(expr: &str) -> Option<f64> {
    let mut p = MathParser {
        chars: expr.chars().filter(|c| !c.is_whitespace()).collect(),
        pos: 0,
    };
    let v = p.expr()?;
    if p.pos != p.chars.len() || !v.is_finite() {
        return None;
    }
    Some(v)
}

struct MathParser {
    chars: Vec<char>,
    pos: usize,
}

impl MathParser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    // expr = term (('+'|'-') term)*
    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        while let Some(op) = self.peek() {
            if op == '+' || op == '-' {
                self.pos += 1;
                let rhs = self.term()?;
                v = if op == '+' { v + rhs } else { v - rhs };
            } else {
                break;
            }
        }
        Some(v)
    }

    // term = factor (('*'|'/'|'%') factor)*
    fn term(&mut self) -> Option<f64> {
        let mut v = self.factor()?;
        while let Some(op) = self.peek() {
            if op == '*' || op == '/' || op == '%' {
                self.pos += 1;
                let rhs = self.factor()?;
                v = match op {
                    '*' => v * rhs,
                    '/' => {
                        if rhs == 0.0 {
                            return None;
                        }
                        v / rhs
                    }
                    _ => {
                        if rhs == 0.0 {
                            return None;
                        }
                        v % rhs
                    }
                };
            } else {
                break;
            }
        }
        Some(v)
    }

    // factor = unary ('^' factor)?
    fn factor(&mut self) -> Option<f64> {
        let base = self.unary()?;
        if self.peek() == Some('^') {
            self.pos += 1;
            let exp = self.factor()?;
            return Some(base.powf(exp));
        }
        Some(base)
    }

    // unary = ('+'|'-')? primary
    fn unary(&mut self) -> Option<f64> {
        match self.peek() {
            Some('-') => {
                self.pos += 1;
                Some(-self.unary()?)
            }
            Some('+') => {
                self.pos += 1;
                self.unary()
            }
            _ => self.primary(),
        }
    }

    // primary = number | '(' expr ')'
    fn primary(&mut self) -> Option<f64> {
        if self.peek() == Some('(') {
            self.pos += 1;
            let v = self.expr()?;
            if self.peek() == Some(')') {
                self.pos += 1;
                return Some(v);
            }
            return None;
        }
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || c == '.' {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.pos == start {
            return None;
        }
        let num: String = self.chars[start..self.pos].iter().collect();
        num.parse().ok()
    }
}

/// Format a float: trim trailing zeros so 5.0 -> "5", 3.50 -> "3.5".
fn format_num(n: f64) -> String {
    if n == n.trunc() && n.abs() < 1e15 {
        return format!("{}", n as i64);
    }
    let s = format!("{:.10}", n);
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    trimmed.to_string()
}

// ---------------------------------------------------------------------------
// Ordinal, length
// ---------------------------------------------------------------------------

fn ordinal(parameter: Option<&str>, payload: &str, name: &str) -> String {
    // Everything after the first '-' (handles negatives like bTagScript).
    let num_str = match payload.find('-') {
        Some(idx) => &payload[idx + 1..],
        None => payload,
    };
    if num_str.is_empty() || !num_str.chars().all(|c| c.is_ascii_digit()) {
        return format!("<{} error>", name);
    }
    let i: i64 = num_str.parse().unwrap_or(0);
    let comma = comma_format(i);
    match parameter {
        Some("c") | Some("comma") => comma,
        Some("i") | Some("indicator") => format!("{}{}", payload, ordinal_suffix(i)),
        _ => format!("{}{}", comma, ordinal_suffix(i)),
    }
}

fn ordinal_suffix(i: i64) -> &'static str {
    let r = i % 10 ;
    let tens = i / 10 % 10 ;
    let start = if tens != 1 && r < 4 { r } else { 0 } as usize;
    const S: &[u8] = b"tsnrhtdd";
    let a = S[start] as char;
    let b = S[start + 4] as char;
    match (a, b) {
        ('s', 't') => "st",
        ('n', 'd') => "nd",
        ('r', 'd') => "rd",
        _ => "th",
    }
}

fn comma_format(n: i64) -> String {
    let neg = n < 0;
    let digits = n.abs().to_string();
    let mut out = String::new();
    let len = digits.len();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if neg { format!("-{}", out) } else { out }
}

fn length_block(parameter: Option<&str>, payload: &str) -> String {
    match parameter {
        None | Some("") => payload.chars().count().to_string(),
        Some("w") | Some("word") | Some("words") => payload.split(' ').count().to_string(),
        Some("s") | Some("space") | Some("spaces") => {
            (payload.split(' ').count() as i64 - 1).to_string()
        }
        _ => "-1".to_string(),
    }
}

// ---------------------------------------------------------------------------
// URL encode / decode
// ---------------------------------------------------------------------------

fn url_encode(s: &str, plus: bool) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        let unreserved = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~');
        // Without `+`, behave like Python `quote` and keep `/` unescaped.
        if unreserved || (!plus && b == b'/') {
            out.push(b as char);
        } else if plus && b == b' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

fn url_decode(s: &str, plus: bool) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                if let Some(h) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    out.push(h);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' if plus => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------------------
// strftime
// ---------------------------------------------------------------------------

/// `spec_order` true: {strftime(format):timestamp}; false: {strf(timestamp):format}.
fn strftime(parameter: Option<&str>, payload: Option<&str>, spec_order: bool) -> String {
    let (fmt, ts): (Option<&str>, Option<&str>) = if spec_order {
        (parameter, payload)
    } else {
        (payload, parameter)
    };

    let fmt = match fmt {
        Some(f) if !f.is_empty() => f,
        _ => return String::new(),
    };

    // Reject invalid format specifiers rather than panicking at format time.
    use chrono::format::{Item, StrftimeItems};
    let items: Vec<Item> = StrftimeItems::new(fmt).collect();
    if items.iter().any(|it| matches!(it, Item::Error)) {
        return String::new();
    }

    let dt: chrono::DateTime<chrono::Utc> = match ts.map(|t| t.trim()).filter(|t| !t.is_empty()) {
        Some(t) if t.chars().all(|c| c.is_ascii_digit()) => {
            match chrono::DateTime::from_timestamp(t.parse::<i64>().unwrap_or(0), 0) {
                Some(d) => d,
                None => return String::new(),
            }
        }
        Some(t) => match parse_iso(t) {
            Some(d) => d,
            None => return String::new(),
        },
        None => chrono::Utc::now(),
    };

    dt.format(fmt).to_string()
}

fn parse_iso(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::{NaiveDateTime, TimeZone, Utc};
    if let Ok(d) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(d.with_timezone(&Utc));
    }
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d",
    ] {
        if let Ok(ndt) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(Utc.from_utc_datetime(&ndt));
        }
        if fmt == "%Y-%m-%d"
            && let Ok(d) = chrono::NaiveDate::parse_from_str(s, fmt) {
                return Some(Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0)?));
            }
    }
    None
}

// ---------------------------------------------------------------------------
// Embed
// ---------------------------------------------------------------------------

fn embed_object(output: &mut TagOutput) -> &mut Map<String, Value> {
    if output.embed.is_none() {
        output.embed = Some(Value::Object(Map::new()));
    }
    match output.embed.as_mut().unwrap() {
        Value::Object(m) => m,
        other => {
            *other = Value::Object(Map::new());
            match other {
                Value::Object(m) => m,
                _ => unreachable!(),
            }
        }
    }
}

fn embed_block(parameter: Option<&str>, payload: Option<&str>, output: &mut TagOutput) {
    // JSON form: {embed({...})} (json in parameter) or payload-only JSON.
    if let Some(p) = parameter {
        let pt = p.trim_start();
        if pt.starts_with('{') {
            merge_embed_json(pt, output);
            return;
        }
    }
    if parameter.is_none() {
        if let Some(p) = payload.map(|p| p.trim())
            && p.starts_with('{') {
                merge_embed_json(p, output);
            }
        return;
    }

    let attr = parameter.unwrap();
    let value = payload.unwrap_or("");
    let map = embed_object(output);
    match attr {
        "title" => {
            map.insert("title".into(), Value::String(value.to_string()));
        }
        "description" => {
            map.insert("description".into(), Value::String(value.to_string()));
        }
        "url" => {
            map.insert("url".into(), Value::String(value.to_string()));
        }
        "color" | "colour" => {
            if let Some(c) = parse_color(value) {
                map.insert("color".into(), Value::from(c));
            }
        }
        "thumbnail" => {
            map.insert("thumbnail".into(), serde_json::json!({ "url": value }));
        }
        "image" => {
            map.insert("image".into(), serde_json::json!({ "url": value }));
        }
        "footer" => {
            map.insert("footer".into(), serde_json::json!({ "text": value }));
        }
        "author" => {
            map.insert("author".into(), serde_json::json!({ "name": value }));
        }
        "field" => {
            let parts = split_pipe(value);
            if parts.len() >= 2 {
                let inline = parts
                    .get(2)
                    .map(|s| s.trim().eq_ignore_ascii_case("true"))
                    .unwrap_or(false);
                let field = serde_json::json!({
                    "name": parts[0],
                    "value": parts[1],
                    "inline": inline,
                });
                match map.entry("fields").or_insert_with(|| Value::Array(vec![])) {
                    Value::Array(arr) => arr.push(field),
                    v => *v = Value::Array(vec![field]),
                }
            }
        }
        _ => {}
    }
}

fn merge_embed_json(text: &str, output: &mut TagOutput) {
    let parsed: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return,
    };
    // Allow a top-level {"embed": {...}} wrapper.
    let obj = match &parsed {
        Value::Object(m) => match m.get("embed") {
            Some(Value::Object(inner)) => inner.clone(),
            _ => m.clone(),
        },
        _ => return,
    };
    let map = embed_object(output);
    for (k, v) in obj {
        let key = if k == "colour" {
            "color".to_string()
        } else {
            k
        };
        map.insert(key, v);
    }
}

/// Parse `#rrggbb`, `0xrrggbb`, a decimal int, or a basic color name -> int.
fn parse_color(value: &str) -> Option<i64> {
    let v = value.trim();
    let hex = v
        .strip_prefix('#')
        .or_else(|| v.strip_prefix("0x"))
        .map(|s| s.to_string());
    if let Some(h) = hex {
        return i64::from_str_radix(h.trim(), 16).ok();
    }
    if let Ok(n) = v.parse::<i64>() {
        return Some(n);
    }
    let named = match v.to_lowercase().as_str() {
        "red" => 0xE74C3C,
        "blue" => 0x3498DB,
        "green" => 0x2ECC71,
        "yellow" => 0xFFFF00,
        "orange" => 0xE67E22,
        "purple" => 0x9B59B6,
        "white" => 0xFFFFFF,
        "black" => 0x000000,
        "gold" => 0xF1C40F,
        "teal" => 0x1ABC9C,
        _ => return None,
    };
    Some(named)
}

// ---------------------------------------------------------------------------
// Cooldown / redirect helpers
// ---------------------------------------------------------------------------

fn cooldown_block(parameter: Option<&str>, payload: Option<&str>, output: &mut TagOutput) {
    let param = parameter.unwrap_or("");
    // Support `seconds` or bTagScript `rate|per` (use `per` as the window).
    let secs_str = param.rsplit('|').next().unwrap_or(param).trim();
    let seconds: u64 = match secs_str.parse::<f64>() {
        Ok(n) if n >= 0.0 => n as u64,
        _ => return,
    };
    // Key is the payload (first `|`-part if a message is appended).
    let key = payload.unwrap_or("");
    let key = key.split('|').next().unwrap_or(key).trim().to_string();
    output.cooldown = Some((key, seconds));
}

/// Extract a u64 channel id from a raw id or `<#123>` mention.
fn extract_channel_id(s: &str) -> Option<u64> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Rebuild an unrecognized block's source text (already-resolved inner content).
fn reconstruct(name: &str, parameter: Option<&str>, payload: Option<&str>) -> String {
    let mut out = String::from("{");
    out.push_str(name);
    if let Some(p) = parameter {
        out.push('(');
        out.push_str(p);
        out.push(')');
    }
    if let Some(p) = payload {
        out.push(':');
        out.push_str(p);
    }
    out.push('}');
    out
}
