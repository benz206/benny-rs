use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct TagContext {
    // Invoking user ({user} / {author})
    pub user_name: String,
    pub user_mention: String,
    pub user_id: String,
    pub user_avatar: String,
    pub user_discriminator: String,
    // First mentioned user, fallback invoker ({target})
    pub target_name: String,
    pub target_mention: String,
    pub target_id: String,
    pub target_avatar: String,
    pub target_discriminator: String,
    // Channel ({channel})
    pub channel_name: String,
    pub channel_id: String,
    pub channel_mention: String,
    // Server / guild ({server} / {guild})
    pub server_name: String,
    pub server_id: String,
    pub server_member_count: String,
    pub server_icon: String,
    // Misc runtime values
    pub args: String,
    pub uses: String,
    // User-assigned variables ({=(name):value})
    pub vars: HashMap<String, String>,
    // Transient engine state (reset each run); set by the {break} block.
    pub break_body: Option<String>,
}

impl TagContext {
    /// Resolve a variable declaration to its value.
    ///
    /// Returns `None` when the declaration is not a known variable (the caller
    /// then leaves the block verbatim, matching bTagScript's loose getter).
    ///
    /// `parameter` / `payload` come from `{name(parameter):payload}` and are
    /// used for string indexing of user vars and `{args(n)}`.
    pub fn get_var(
        &self,
        name: &str,
        parameter: Option<&str>,
        payload: Option<&str>,
    ) -> Option<String> {
        // 1. User-defined variables behave like bTagScript StringAdapters.
        if let Some(v) = self.vars.get(name) {
            return Some(string_index(v, parameter, payload));
        }

        // 2. `{args}` / `{args(n)}` / `{args(n+)}` — indexable like a StringAdapter.
        if name == "args" {
            return Some(string_index(&self.args, parameter, payload));
        }

        // 3. Simple scalar runtime variables.
        match name {
            "uses" => return Some(self.uses.clone()),
            "unix" => {
                return Some(chrono::Utc::now().timestamp().to_string());
            }
            _ => {}
        }

        // 4. Discord attribute variables. Accept both dot (`{user.id}`) and
        //    parenthesis (`{user(id)}`) attribute syntax.
        let (base, attr): (&str, Option<&str>) = if let Some((b, a)) = name.split_once('.') {
            (b, Some(a))
        } else {
            (name, parameter)
        };

        let attr = attr.map(|a| a.trim());

        match base {
            "user" | "author" => Some(self.user_attr(attr)),
            "target" => Some(self.target_attr(attr)),
            "channel" => Some(self.channel_attr(attr)),
            "server" | "guild" => Some(self.server_attr(attr)),
            _ => None,
        }
    }

    fn user_attr(&self, attr: Option<&str>) -> String {
        match attr {
            None | Some("") | Some("name") | Some("nick") => self.user_name.clone(),
            Some("id") => self.user_id.clone(),
            Some("mention") => self.user_mention.clone(),
            Some("avatar") => self.user_avatar.clone(),
            Some("discriminator") => self.user_discriminator.clone(),
            _ => String::new(),
        }
    }

    fn target_attr(&self, attr: Option<&str>) -> String {
        match attr {
            None | Some("") | Some("name") | Some("nick") => self.target_name.clone(),
            Some("id") => self.target_id.clone(),
            Some("mention") => self.target_mention.clone(),
            Some("avatar") => self.target_avatar.clone(),
            Some("discriminator") => self.target_discriminator.clone(),
            _ => String::new(),
        }
    }

    fn channel_attr(&self, attr: Option<&str>) -> String {
        match attr {
            None | Some("") | Some("name") => self.channel_name.clone(),
            Some("id") => self.channel_id.clone(),
            Some("mention") => self.channel_mention.clone(),
            _ => String::new(),
        }
    }

    fn server_attr(&self, attr: Option<&str>) -> String {
        match attr {
            None | Some("") | Some("name") => self.server_name.clone(),
            Some("id") => self.server_id.clone(),
            Some("member_count") | Some("members") => self.server_member_count.clone(),
            Some("icon") => self.server_icon.clone(),
            _ => String::new(),
        }
    }
}

/// Port of bTagScript's `StringAdapter.handle_ctx`: index/slice a string by a
/// 1-based parameter, splitting on whitespace (or a custom `payload` splitter).
///
/// Supports `{var(n)}` (nth item), `{var(-n)}` (from the end), `{var(+n)}`
/// (join up to n) and `{var(n+)}` (join from n onward). On any failure the
/// whole string is returned, matching the reference implementation.
fn string_index(s: &str, parameter: Option<&str>, payload: Option<&str>) -> String {
    let param = match parameter {
        None => return s.to_string(),
        Some(p) => p,
    };

    let splitter = match payload {
        None | Some("") => " ",
        Some(p) => p,
    };
    let parts: Vec<&str> = s.split(splitter).collect();
    let len = parts.len() as i64;

    // Python-style index resolution (supports negatives).
    let at = |i: i64| -> Option<usize> {
        let idx = if i < 0 { len + i } else { i };
        if idx >= 0 && idx < len {
            Some(idx as usize)
        } else {
            None
        }
    };

    let trimmed = param.trim();

    // Plain integer index.
    if let Ok(v) = trimmed.parse::<i64>() {
        let i = if trimmed.starts_with('-') { v } else { v - 1 };
        return at(i).map(|u| parts[u].to_string()).unwrap_or_else(|| s.to_string());
    }

    // `+n` (join head) / `n+` (join tail).
    let stripped = trimmed.replace('+', "");
    if let Ok(v) = stripped.parse::<i64>() {
        let i = if v > 0 { v - 1 } else { v };
        if trimmed.starts_with('+') {
            let end = at(i).map(|u| u + 1).unwrap_or(0);
            return parts[..end.min(parts.len())].join(splitter);
        }
        if trimmed.ends_with('+') {
            let start = at(i).unwrap_or(0);
            return parts[start..].join(splitter);
        }
    }

    s.to_string()
}
