mod lexer;
mod blocks;
pub mod context;

pub use context::TagContext;

#[derive(Debug, Clone, Default)]
pub struct TagOutput {
    pub content: String,
    pub react_emojis: Vec<String>,
    pub delete_invoke: bool,
    pub redirect_channel: Option<u64>,
}

pub fn run(template: &str, ctx: &mut TagContext) -> TagOutput {
    let tokens = lexer::tokenize(template);
    let mut output = TagOutput::default();
    let mut content_parts = Vec::new();

    for token in tokens {
        match token {
            lexer::Token::Literal(s) => content_parts.push(s),
            lexer::Token::Block { name, args, body } => {
                let result = blocks::process_block(&name, &args, &body, ctx, &mut output);
                content_parts.push(result);
            }
        }
    }

    output.content = content_parts.join("");
    output
}
