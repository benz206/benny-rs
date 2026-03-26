use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct TagContext {
    pub user_name: String,
    pub user_mention: String,
    pub user_id: String,
    pub user_avatar: String,
    pub server_name: String,
    pub server_id: String,
    pub server_member_count: String,
    pub channel_name: String,
    pub channel_id: String,
    pub args: String,
    pub vars: HashMap<String, String>,
}

impl TagContext {
    pub fn get_var(&self, name: &str) -> Option<String> {
        // Check user-set variables first
        if let Some(v) = self.vars.get(name) {
            return Some(v.clone());
        }
        // Then check built-in variables
        match name {
            "user" | "user.name" => Some(self.user_name.clone()),
            "user.mention" => Some(self.user_mention.clone()),
            "user.id" => Some(self.user_id.clone()),
            "user.avatar" => Some(self.user_avatar.clone()),
            "server" | "server.name" => Some(self.server_name.clone()),
            "server.id" => Some(self.server_id.clone()),
            "server.member_count" => Some(self.server_member_count.clone()),
            "channel" | "channel.name" => Some(self.channel_name.clone()),
            "channel.id" => Some(self.channel_id.clone()),
            "args" => Some(self.args.clone()),
            _ => None,
        }
    }
}
