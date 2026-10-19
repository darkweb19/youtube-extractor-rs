//! Minimal, bounded recognizer for a narrow subset of public player signature transforms.
//! Never evaluate downloaded JavaScript. Unsupported patterns fail closed.
use crate::error::{AppError, ErrorCode, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Op {
    Reverse,
    Drop(usize),
    Swap(usize),
}

#[derive(Debug, Clone)]
pub struct SignatureAlgorithm {
    steps: Vec<Op>,
}

impl SignatureAlgorithm {
    pub fn parse(script: &str) -> Option<Self> {
        if script.len() > 4 * 1024 * 1024 {
            return None;
        }
        let mut rest = script;
        let mut candidates = Vec::new();
        for _ in 0..4096 {
            let Some(pos) = rest.find("function(") else {
                break;
            };
            rest = &rest[pos + 9..];
            let Some(close) = rest.find(')') else {
                break;
            };
            if close > 32 {
                continue;
            }
            let parameter = &rest[..close];
            if !ident(parameter) {
                continue;
            }
            let after = rest[close + 1..].trim_start();
            if let Some(body) = block(after, '{', '}') {
                if body.len() <= 2048
                    && body.contains(".split(\"\")")
                    && body.contains(".join(\"\")")
                {
                    if let Some(steps) = steps(body, parameter, script) {
                        if !steps.is_empty() && steps.len() <= 32 {
                            candidates.push(steps);
                        }
                    }
                }
            }
        }
        if candidates.len() == 1 {
            Some(Self {
                steps: candidates.remove(0),
            })
        } else {
            None
        }
    }

    pub fn decipher(&self, signature: &str) -> Result<String> {
        if signature.is_empty()
            || signature.len() > 256
            || !signature.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(unsupported("Unsupported signature value"));
        }
        let mut value = signature.as_bytes().to_vec();
        for step in &self.steps {
            match step {
                Op::Reverse => value.reverse(),
                Op::Drop(n) => {
                    value.drain(..(*n).min(value.len()));
                }
                Op::Swap(n) => {
                    if !value.is_empty() {
                        let i = *n % value.len();
                        value.swap(0, i);
                    }
                }
            }
        }
        if value.is_empty() {
            return Err(unsupported("Signature decoder produced empty output"));
        }
        String::from_utf8(value).map_err(|_| unsupported("Invalid decoded signature"))
    }
}

fn unsupported(message: &str) -> AppError {
    AppError::new(ErrorCode::UnsupportedFormat, message)
}

fn ident(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 24
        && !s.as_bytes()[0].is_ascii_digit()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$')
}

fn block(input: &str, open: char, close: char) -> Option<&str> {
    if !input.starts_with(open) {
        return None;
    }
    let (mut depth, mut quote, mut escaped) = (0usize, None, false);
    for (i, ch) in input.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == q {
                quote = None;
            }
            continue;
        }
        if ch == '"' || ch == '\'' {
            quote = Some(ch);
            continue;
        }
        if ch == open {
            depth += 1;
        }
        if ch == close {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return input.get(..=i);
            }
        }
    }
    None
}

fn steps(body: &str, param: &str, script: &str) -> Option<Vec<Op>> {
    let mut statements = body
        .trim_start_matches('{')
        .trim_end_matches('}')
        .split(';');
    if statements.next()?.trim() != format!("{param}={param}.split(\"\")") {
        return None;
    }
    let mut result = Vec::new();
    let mut joined = false;
    for stmt in statements {
        let stmt = stmt.trim();
        if stmt.is_empty() {
            continue;
        }
        if stmt == format!("return {param}.join(\"\")") {
            joined = true;
            continue;
        }
        if joined || result.len() >= 32 {
            return None;
        }
        let (call, args) = stmt.split_once('(')?;
        let args = args.strip_suffix(')')?;
        let (obj, method) = call.split_once('.')?;
        if !ident(obj) || !ident(method) {
            return None;
        }
        let mut arg_parts = args.split(',').map(str::trim);
        if arg_parts.next()? != param {
            return None;
        }
        let number = arg_parts.next().map(str::parse::<usize>).transpose().ok()?;
        if arg_parts.next().is_some() || number.is_some_and(|n| n > 256) {
            return None;
        }
        result.push(helper(script, obj, method, number)?);
    }
    joined.then_some(result)
}

fn helper(script: &str, obj: &str, method: &str, number: Option<usize>) -> Option<Op> {
    let mut definitions = Vec::new();
    for assignment in [format!("{obj}={{"), format!("{obj} = {{")] {
        let mut rest = script;
        for _ in 0..16 {
            let Some(i) = rest.find(&assignment) else {
                break;
            };
            rest = &rest[i + assignment.len() - 1..];
            if let Some(object) = block(rest, '{', '}') {
                if object.len() <= 4096 {
                    definitions.push(object);
                }
            }
            rest = &rest[1..];
        }
    }
    if definitions.len() != 1 {
        return None;
    }
    let obj_body = definitions[0];
    let mut methods = Vec::new();
    for key in [
        format!("{method}:function("),
        format!("{method}: function("),
        format!("\"{method}\":function("),
        format!("\"{method}\": function("),
    ] {
        if let Some(i) = obj_body.find(&key) {
            let tail = &obj_body[i + key.len()..];
            let close = tail.find(')')?;
            if close > 32 {
                return None;
            }
            let args = tail[..close].to_string();
            let after = tail[close + 1..].trim_start();
            if let Some(code) = block(after, '{', '}') {
                if code.len() <= 500 {
                    methods.push((args, code));
                }
            }
        }
    }
    if methods.len() != 1 {
        return None;
    }
    let (args, body) = methods[0].clone();
    let args: Vec<&str> = args.split(',').map(str::trim).collect();
    if args.is_empty() || args.len() > 2 || !args.iter().all(|arg| ident(arg)) {
        return None;
    }
    let first = args[0];
    let compact: String = body
        .trim_start_matches('{')
        .trim_end_matches('}')
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let compact = compact.trim_end_matches(';');
    if compact == format!("{first}.reverse()") {
        return Some(Op::Reverse);
    }
    let second = *args.get(1)?;
    if compact == format!("{first}.splice(0,{second})") {
        return number.map(Op::Drop);
    }
    let source = compact
        .strip_prefix("var")
        .or_else(|| compact.strip_prefix("let"))?;
    let (temporary, _) = source.split_once('=')?;
    if !ident(temporary) {
        return None;
    }
    if source
        == format!(
            "{temporary}={first}[0];{first}[0]={first}[{second}%{first}.length];{first}[{second}]={temporary}"
        )
    {
        number.map(Op::Swap)
    } else {
        None
    }
}

/// Only allowlisted same-origin public player scripts can be fetched.
pub fn player_script_url(html: &str) -> Option<String> {
    if html.len() > 4 * 1024 * 1024 {
        return None;
    }
    let mut rest = html;
    for _ in 0..16 {
        let Some(i) = rest.find("/s/player/") else {
            break;
        };
        rest = &rest[i..];
        if let Some(end) = rest.find(".js") {
            let end = end + 3;
            if end <= 180 {
                let path = &rest[..end];
                if path.len() >= 24
                    && path.ends_with(".js")
                    && path
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-/".contains(&b))
                    && !path.contains("..")
                {
                    return Some(format!("https://www.youtube.com{path}"));
                }
            }
        }
        rest = &rest[1..];
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"var AL={RV:function(a){a.reverse()},DR:function(a,b){a.splice(0,b)},SW:function(a,b){var c=a[0];a[0]=a[b%a.length];a[b]=c}};XY=function(a){a=a.split("");AL.SW(a,2);AL.RV(a);AL.DR(a,1);return a.join("")};"#;

    #[test]
    fn resolves_recognized_operations() {
        let algo = SignatureAlgorithm::parse(SAMPLE).unwrap();
        assert_eq!(algo.decipher("abcdef").unwrap(), "edabc");
    }

    #[test]
    fn refuses_unrecognized_or_ambiguous_player_functions() {
        assert!(SignatureAlgorithm::parse(&SAMPLE.replace("AL.DR(a,1)", "AL.X(a,1)")).is_none());
        assert!(SignatureAlgorithm::parse(&(SAMPLE.to_owned() + SAMPLE)).is_none());
        assert!(
            SignatureAlgorithm::parse(&SAMPLE.replace("a.reverse()", "a.reverse();a.push(1)"))
                .is_none()
        );
    }

    #[test]
    fn only_accepts_player_script_paths() {
        assert_eq!(
            player_script_url(
                r#"<script src="/s/player/cafebabe/player_ias.vflset/en_US/base.js"></script>"#
            )
            .as_deref(),
            Some("https://www.youtube.com/s/player/cafebabe/player_ias.vflset/en_US/base.js")
        );
        assert!(
            player_script_url(r#"<script src="/s/player/../evil/base.js"></script>"#).is_none()
        );
    }
}
