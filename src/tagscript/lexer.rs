#[derive(Debug, Clone)]
pub enum Token {
    Literal(String),
    Block {
        name: String,
        args: Vec<String>,
        body: String,
    },
}

pub fn tokenize(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();
    let mut current_literal = String::new();

    while let Some(&ch) = chars.peek() {
        if ch == '{' {
            chars.next();
            // Check for escaped brace
            if chars.peek() == Some(&'{') {
                chars.next();
                current_literal.push('{');
                continue;
            }
            // Read until matching closing brace
            let mut block_content = String::new();
            let mut depth = 1;
            loop {
                match chars.next() {
                    None => {
                        // Unclosed block — treat as literal
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

fn parse_block(content: &str) -> Token {
    // Format: name(arg1,arg2,...):body  OR  name:body  OR  =name:body (assign)
    let content = content.trim();

    // Handle assignment: {=(varname):value}
    if content.starts_with('=') {
        let rest = &content[1..];
        if let Some((name, body)) = rest.split_once(':') {
            return Token::Block {
                name: "=".to_string(),
                args: vec![name.trim().to_string()],
                body: body.to_string(),
            };
        }
    }

    // Try to find (args) part before the colon
    // Format: name(arg1,arg2):body or name:body
    let (name_and_args, body) = if let Some(colon_pos) = content.find(':') {
        (&content[..colon_pos], &content[colon_pos + 1..])
    } else {
        (content, "")
    };

    let (name, args) = if let Some(paren_start) = name_and_args.find('(') {
        if name_and_args.ends_with(')') {
            let name = &name_and_args[..paren_start];
            let args_str = &name_and_args[paren_start + 1..name_and_args.len() - 1];
            let args: Vec<String> = args_str.split(',').map(|s| s.trim().to_string()).collect();
            (name, args)
        } else {
            (name_and_args, vec![])
        }
    } else {
        (name_and_args, vec![])
    };

    Token::Block {
        name: name.trim().to_string(),
        args,
        body: body.to_string(),
    }
}
