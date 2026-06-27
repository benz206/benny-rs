#[derive(Debug, Clone)]
pub enum Token {
    Literal(String),
    Block {
        /// Block name, e.g. `if`, `=`, `user.id`.
        declaration: String,
        /// Text inside the parentheses, e.g. `cond` in `{if(cond):...}`.
        parameter: Option<String>,
        /// Text after the top-level colon, e.g. `a|b` in `{if(cond):a|b}`.
        payload: Option<String>,
    },
}

/// Split `input` into literals and brace blocks. Nested `{...}` are kept whole
/// inside a block's parameter/payload (the interpreter resolves them inner-first).
/// `{{` and `}}` are unescaped to literal braces.
pub fn tokenize(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();
    let mut current_literal = String::new();

    while let Some(&ch) = chars.peek() {
        if ch == '{' {
            chars.next();
            // Escaped opening brace `{{` -> literal `{`.
            if chars.peek() == Some(&'{') {
                chars.next();
                current_literal.push('{');
                continue;
            }
            // Read until the matching closing brace, tracking nesting depth.
            let mut block_content = String::new();
            let mut depth = 1;
            loop {
                match chars.next() {
                    None => {
                        // Unclosed block — treat the whole thing as literal.
                        current_literal.push('{');
                        current_literal.push_str(&block_content);
                        break;
                    }
                    Some('}') => {
                        depth -= 1;
                        if depth == 0 {
                            if !current_literal.is_empty() {
                                tokens.push(Token::Literal(std::mem::take(&mut current_literal)));
                            }
                            tokens.push(parse_block(&block_content));
                            break;
                        } else {
                            block_content.push('}');
                        }
                    }
                    Some('{') => {
                        depth += 1;
                        block_content.push('{');
                    }
                    Some(c) => block_content.push(c),
                }
            }
        } else if ch == '}' {
            chars.next();
            // Escaped closing brace `}}` -> literal `}`.
            if chars.peek() == Some(&'}') {
                chars.next();
            }
            current_literal.push('}');
        } else {
            current_literal.push(ch);
            chars.next();
        }
    }

    if !current_literal.is_empty() {
        tokens.push(Token::Literal(current_literal));
    }
    tokens
}

/// Parse the inside of a `{...}` block into declaration / parameter / payload.
///
/// Mirrors bTagScript's `Verb.__parse`: the first balanced `(...)` group forms
/// the parameter; a colon at paren-depth 0 starts the payload. A backslash
/// escapes the next character so it can't terminate the parameter/payload.
fn parse_block(content: &str) -> Token {
    let chars: Vec<char> = content.chars().collect();
    let mut depth = 0i32;
    let mut param_start: Option<usize> = None;
    let mut declaration: Option<String> = None;
    let mut parameter: Option<String> = None;
    let mut payload: Option<String> = None;
    let mut skip = false;

    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if skip {
            skip = false;
            i += 1;
            continue;
        }
        match c {
            '\\' => {
                skip = true;
            }
            ':' if depth == 0 => {
                // Top-level colon: everything before is the declaration,
                // everything after is the payload.
                let decl: String = chars[..i].iter().collect();
                declaration = Some(decl);
                payload = Some(chars[i + 1..].iter().collect());
                return finish(declaration, parameter, payload);
            }
            '(' => {
                if depth == 0 && param_start.is_none() {
                    param_start = Some(i);
                    declaration = Some(chars[..i].iter().collect());
                }
                depth += 1;
            }
            ')' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    if let Some(start) = param_start {
                        parameter = Some(chars[start + 1..i].iter().collect());
                    }
                    // A colon immediately after the parameter starts the payload.
                    if chars.get(i + 1) == Some(&':') {
                        payload = Some(chars[i + 2..].iter().collect());
                    }
                    return finish(declaration, parameter, payload);
                }
            }
            _ => {}
        }
        i += 1;
    }

    // No top-level colon and no closed parameter: the whole content is the name.
    let decl: String = content.to_string();
    finish(Some(decl), parameter, payload)
}

fn finish(
    declaration: Option<String>,
    parameter: Option<String>,
    payload: Option<String>,
) -> Token {
    Token::Block {
        declaration: declaration.unwrap_or_default(),
        parameter,
        payload,
    }
}
