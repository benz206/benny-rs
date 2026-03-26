use super::{TagOutput, context::TagContext};
use rand::Rng;

pub fn process_block(
    name: &str,
    args: &[String],
    body: &str,
    ctx: &mut TagContext,
    output: &mut TagOutput,
) -> String {
    match name {
        // Assignment: {=(varname):value}
        "=" => {
            let var_name = args.first().cloned().unwrap_or_default();
            if !var_name.is_empty() {
                ctx.vars.insert(var_name, body.to_string());
            }
            String::new()
        }

        // Random choice: {choose:a|b|c}
        "choose" => {
            let choices: Vec<&str> = body.split('|').collect();
            if choices.is_empty() {
                return String::new();
            }
            let idx = rand::thread_rng().gen_range(0..choices.len());
            choices[idx].trim().to_string()
        }

        // Random range: {range(min,max)} or {range:max}
        "range" => {
            let (min, max) = if args.len() >= 2 {
                let min: i64 = args[0].parse().unwrap_or(0);
                let max: i64 = args[1].parse().unwrap_or(100);
                (min, max)
            } else {
                let max: i64 = body.parse().unwrap_or(100);
                (0, max)
            };
            if min >= max {
                return min.to_string();
            }
            rand::thread_rng().gen_range(min..=max).to_string()
        }

        // Conditional: {if(condition):true|false}
        "if" => {
            let condition = args.first().cloned().unwrap_or_default();
            let parts: Vec<&str> = body.splitn(2, '|').collect();
            let true_val = parts.first().copied().unwrap_or("");
            let false_val = parts.get(1).copied().unwrap_or("");
            let is_true = !condition.is_empty()
                && condition != "0"
                && condition != "false"
                && condition != "no";
            if is_true {
                true_val.to_string()
            } else {
                false_val.to_string()
            }
        }

        // React side-effect: {react:emoji}
        "react" => {
            if !body.is_empty() {
                output.react_emojis.push(body.to_string());
            }
            String::new()
        }

        // Delete the invocation: {delete}
        "delete" => {
            output.delete_invoke = true;
            String::new()
        }

        // Redirect output to another channel: {redirect:channel_id}
        "redirect" => {
            if let Ok(id) = body.parse::<u64>() {
                output.redirect_channel = Some(id);
            }
            String::new()
        }

        // Comment: {# comment text} — outputs nothing
        "#" => String::new(),

        // Variable access or unknown block
        n => {
            // Try built-in variable lookup first, then user-defined vars
            ctx.get_var(n).unwrap_or_default()
        }
    }
}
