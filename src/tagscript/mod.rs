mod blocks;
pub mod context;
mod lexer;

pub use context::TagContext;

#[derive(Debug, Clone, Default)]
pub struct TagOutput {
    /// Rendered text output of the tag.
    pub content: String,
    /// Emojis requested via `{react:...}` (in order, deduped by the cog).
    pub react_emojis: Vec<String>,
    /// `{delete}` — delete the invoking message.
    pub delete_invoke: bool,
    /// `{redirect:<id>}` — send the response to this channel id instead.
    pub redirect_channel: Option<u64>,
    /// Accumulated Discord embed built by `{embed(...)}` blocks. JSON object
    /// whose keys are a subset of: `title`, `description`, `color` (int),
    /// `url`, `fields` (array of `{name,value,inline}`), `thumbnail` (`{url}`),
    /// `image` (`{url}`), `footer` (`{text}`), `author` (`{name,icon_url}`).
    pub embed: Option<serde_json::Value>,
    /// `{cd(seconds):key}` — (key, seconds) cooldown the cog must enforce.
    pub cooldown: Option<(String, u64)>,
    /// `{stop:msg}` halted processing; `content` holds the stop message.
    pub stopped: bool,
}

/// Render a TagScript template against `ctx`, producing text + side effects.
pub fn run(template: &str, ctx: &mut TagContext) -> TagOutput {
    ctx.break_body = None;
    let mut output = TagOutput::default();

    let content = resolve(template, ctx, &mut output);

    if output.stopped {
        // `output.content` was already set to the stop message.
    } else if let Some(body) = ctx.break_body.take() {
        output.content = body;
    } else {
        output.content = content;
    }
    output
}

/// Recursively render a string: literals pass through, blocks have their
/// parameter and payload resolved inner-first, then are dispatched.
pub(crate) fn resolve(input: &str, ctx: &mut TagContext, output: &mut TagOutput) -> String {
    let tokens = lexer::tokenize(input);
    let mut parts = Vec::new();

    for token in tokens {
        if output.stopped {
            break;
        }
        match token {
            lexer::Token::Literal(s) => parts.push(s),
            lexer::Token::Block {
                declaration,
                parameter,
                payload,
            } => {
                // Resolve nested blocks inside the parameter/payload first.
                let parameter = parameter.map(|p| resolve(&p, ctx, output));
                let payload = payload.map(|p| resolve(&p, ctx, output));
                let result = blocks::process_block(
                    &declaration,
                    parameter.as_deref(),
                    payload.as_deref(),
                    ctx,
                    output,
                );
                parts.push(result);
            }
        }
    }

    parts.join("")
}
