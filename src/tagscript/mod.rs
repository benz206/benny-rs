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

/// Hardening bounds. Templates (tags, welcome/goodbye) are author-controlled and
/// run in the shared bot process, so a deeply nested or runaway template could
/// otherwise stack-overflow or hang the whole bot.
/// Max nesting depth of `{...}` blocks before a sub-render is left verbatim.
const MAX_DEPTH: u32 = 50;
/// Max total blocks dispatched per run before further blocks are skipped.
const MAX_NODES: u32 = 5_000;
/// Max accumulated output (chars) per render level before it is truncated.
const MAX_OUTPUT: usize = 10_000;

/// TEMPORARILY DISABLED. The TagScript engine has known correctness/security
/// issues — most importantly the `{require}`/`{whitelist}`/`{blacklist}` access
/// gates are no-ops, so a tag that should be restricted instead *fails open*.
/// Until those are fixed the engine is switched off at the entry point: every
/// template is returned verbatim with no block evaluation and no side effects.
/// Flip this back to `true` to re-enable the engine.
const ENGINE_ENABLED: bool = false;

/// Render a TagScript template against `ctx`, producing text + side effects.
pub fn run(template: &str, ctx: &mut TagContext) -> TagOutput {
    if !ENGINE_ENABLED {
        // Inert passthrough: emit the raw template, drop all `{...}` evaluation.
        return TagOutput {
            content: template.to_owned(),
            ..TagOutput::default()
        };
    }

    ctx.break_body = None;
    ctx.nodes = 0;
    let mut output = TagOutput::default();

    let content = resolve(template, ctx, &mut output, 0);

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
///
/// `depth` is the current block-nesting level; together with the shared
/// `ctx.nodes` work counter and the per-level output cap it bounds total work so
/// an author-controlled template cannot stack-overflow, hang, or balloon memory.
pub(crate) fn resolve(
    input: &str,
    ctx: &mut TagContext,
    output: &mut TagOutput,
    depth: u32,
) -> String {
    // Too deeply nested: leave this sub-render verbatim instead of recursing.
    if depth > MAX_DEPTH {
        return input.to_string();
    }
    let tokens = lexer::tokenize(input);
    let mut parts = Vec::new();
    let mut total = 0usize;

    for token in tokens {
        if output.stopped {
            break;
        }
        match token {
            lexer::Token::Literal(s) => {
                total += s.len();
                parts.push(s);
            }
            lexer::Token::Block {
                declaration,
                parameter,
                payload,
            } => {
                // Bound total work across width as well as depth: once the node
                // budget is spent, stop dispatching further blocks.
                ctx.nodes += 1;
                if ctx.nodes > MAX_NODES {
                    break;
                }
                // Resolve nested blocks inside the parameter/payload first.
                let parameter = parameter.map(|p| resolve(&p, ctx, output, depth + 1));
                let payload = payload.map(|p| resolve(&p, ctx, output, depth + 1));
                let result = blocks::process_block(
                    &declaration,
                    parameter.as_deref(),
                    payload.as_deref(),
                    ctx,
                    output,
                );
                total += result.len();
                parts.push(result);
            }
        }
        // Cap accumulated output per render level to avoid a giant string.
        if total > MAX_OUTPUT {
            break;
        }
    }

    parts.join("")
}
