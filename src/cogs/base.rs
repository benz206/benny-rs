use super::Cog;
use async_trait::async_trait;
use serenity::all::{Context, Message};
use std::sync::Arc;
use std::{fs, path::Path};

pub struct BaseCog {
    prefix: String,
}

impl BaseCog {
    pub fn new(prefix: String) -> Arc<Self> { Arc::new(Self { prefix }) }

    fn count_files_and_lines(root: &Path) -> (u64, u64) {
        let mut files = 0u64;
        let mut lines = 0u64;
        let mut stack = vec![root.to_path_buf()];
        while let Some(p) = stack.pop() {
            if let Ok(rd) = fs::read_dir(&p) {
                for entry in rd.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                    } else if path.is_file() {
                        files += 1;
                        if let Ok(text) = fs::read_to_string(&path) {
                            lines += text.lines().count() as u64;
                        }
                    }
                }
            }
        }
        (files, lines)
    }
}

#[async_trait]
impl Cog for BaseCog {
    async fn on_message(&self, ctx: &Context, msg: &Message) {
        if msg.author.bot { return; }
        let content = msg.content.trim();
        if !content.starts_with(&self.prefix) { return; }
        let rest = &content[self.prefix.len()..];
        let mut it = rest.split_whitespace();
        let Some(cmd) = it.next() else { return };
        match cmd {
            "ping" => { let _ = msg.channel_id.say(&ctx.http, "Pong!").await; }
            "about" => { let _ = msg.channel_id.say(&ctx.http, "Benny-rs bot (scaffold)").await; }
            "files" => {
                let (f, l) = Self::count_files_and_lines(Path::new("."));
                let _ = msg.channel_id.say(&ctx.http, format!("Files: {f}, Lines: {l}")).await;
            }
            _ => {}
        }
    }
}


