use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use serde_json::{Number, Value};

use crate::{
    ConfigSourceError, MAX_NODES, MAX_STRUCTURAL_DEPTH, SourceOverrides, limits::check_string,
};

mod eval;
mod parser;
mod render;

pub(crate) fn decode(source: &str) -> Result<Value, ConfigSourceError> {
    decode_inner(source, None, None).map(|(value, _, _)| value)
}

pub(crate) fn decode_with_base_dir(
    source: &str,
    base_dir: &Path,
    overrides: Option<&SourceOverrides>,
) -> Result<(Value, Vec<PathBuf>, bool), ConfigSourceError> {
    decode_inner(source, Some(base_dir), overrides)
}

fn decode_inner(
    source: &str,
    base_dir: Option<&Path>,
    overrides: Option<&SourceOverrides>,
) -> Result<(Value, Vec<PathBuf>, bool), ConfigSourceError> {
    let parsed = parser::parse(source)?;
    eval::evaluate(parsed, base_dir, overrides)
}

pub(crate) fn render(value: &Value) -> Result<String, ConfigSourceError> {
    render::render(value)
}

fn parse_err(line: u32, message: impl Into<String>) -> ConfigSourceError {
    ConfigSourceError::parse_at("OXI", message, Some(line))
}

struct ParsedDocument {
    items: Vec<RootItem>,
}

enum RootItem {
    Entry {
        key: String,
        value: Expr,
        line: u32,
    },
    Include {
        path: String,
        line: u32,
    },
    LuaBlock {
        code: String,
        line: u32,
    },
    Site {
        name: String,
        body: Expr,
        line: u32,
    },
    Template {
        name: String,
        body: Expr,
        line: u32,
    },
    NamedCollection {
        collection: &'static str,
        name: String,
        body: Expr,
        line: u32,
    },
    NativeServer {
        key: &'static str,
        value: Expr,
        line: u32,
    },
    Use {
        values: Vec<Expr>,
        line: u32,
    },
}

#[derive(Clone, Debug)]
enum Expr {
    Null,
    Bool(bool),
    Number(Number),
    String(String),
    Array(Vec<Expr>),
    Object(Vec<(String, Expr, u32)>),
    LuaInline(String),
    LuaBlock(String),
}

#[derive(Clone, Debug)]
enum Token {
    Bare(String),
    Quoted(String),
    InlineArray(Vec<Token>),
    EmptyObject,
    LuaExpr(String),
    Kv(String, Box<Token>),
}

#[derive(Default)]
struct ObjectBuilder {
    entries: Vec<(String, Expr, u32)>,
    seen: HashSet<String>,
    use_values: Vec<Expr>,
    use_line: Option<u32>,
    routes: Vec<Expr>,
    route_line: Option<u32>,
}

impl ObjectBuilder {
    fn insert(&mut self, key: String, value: Expr, line: u32) -> Result<(), ConfigSourceError> {
        if !self.seen.insert(key.clone()) {
            return Err(parse_err(line, format!("duplicate key `{key}`")));
        }
        self.entries.push((key, value, line));
        Ok(())
    }

    fn push_use(&mut self, mut vals: Vec<Expr>, line: u32) {
        self.use_line.get_or_insert(line);
        self.use_values.append(&mut vals);
    }

    fn push_route(&mut self, route: Expr, line: u32) {
        self.route_line.get_or_insert(line);
        self.routes.push(route);
    }

    fn finish(mut self, nodes: &mut usize) -> Result<Expr, ConfigSourceError> {
        let mut use_values = std::mem::take(&mut self.use_values);
        if !use_values.is_empty() {
            let line = self.use_line.unwrap_or(1);
            let value = if use_values.len() == 1 {
                use_values.pop().unwrap_or(Expr::Null)
            } else {
                bump_shared_node(nodes)?;
                Expr::Array(use_values)
            };
            self.insert("use".to_owned(), value, line)?;
        }
        let routes = std::mem::take(&mut self.routes);
        if !routes.is_empty() {
            let line = self.route_line.unwrap_or(1);
            if let Some((_, existing, _)) = self.entries.iter_mut().find(|(k, _, _)| k == "routes")
            {
                if let Expr::Array(arr) = existing {
                    arr.extend(routes);
                } else {
                    return Err(parse_err(
                        line,
                        "`route` directive conflicts with non-array `routes` key",
                    ));
                }
            } else {
                bump_shared_node(nodes)?;
                self.insert("routes".to_owned(), Expr::Array(routes), line)?;
            }
        }
        Ok(Expr::Object(self.entries))
    }
}

fn bump_shared_node(nodes: &mut usize) -> Result<(), ConfigSourceError> {
    *nodes = nodes.checked_add(1).ok_or(ConfigSourceError::NodeLimit)?;
    if *nodes > MAX_NODES {
        return Err(ConfigSourceError::NodeLimit);
    }
    Ok(())
}

fn is_valid_int(raw: &str) -> bool {
    if raw == "-0" {
        return false;
    }
    let digits = raw.strip_prefix('-').unwrap_or(raw);
    !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && !(digits.len() > 1 && digits.starts_with('0'))
}

fn is_valid_float(raw: &str) -> bool {
    let body = raw.strip_prefix('-').unwrap_or(raw);
    let Some((int_part, rest)) = body.split_once('.') else {
        return false;
    };
    if int_part.is_empty()
        || !int_part.bytes().all(|b| b.is_ascii_digit())
        || (int_part.len() > 1 && int_part.starts_with('0'))
    {
        return false;
    }
    let (frac_part, exp_part) = match rest.find(['e', 'E']) {
        Some(pos) => (&rest[..pos], Some(&rest[pos + 1..])),
        None => (rest, None),
    };
    if frac_part.is_empty() || !frac_part.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    if let Some(exp) = exp_part {
        let exp_digits = exp
            .strip_prefix('+')
            .or_else(|| exp.strip_prefix('-'))
            .unwrap_or(exp);
        if exp_digits.is_empty() || !exp_digits.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
    }
    true
}

fn bare_to_expr(raw: &str) -> Result<Expr, ConfigSourceError> {
    match raw {
        "true" | "on" => return Ok(Expr::Bool(true)),
        "false" | "off" => return Ok(Expr::Bool(false)),
        "null" | "none" => return Ok(Expr::Null),
        _ => {}
    }
    if is_valid_int(raw) {
        if let Ok(value) = raw.parse::<i64>() {
            return Ok(Expr::Number(Number::from(value)));
        }
        if !raw.starts_with('-')
            && let Ok(value) = raw.parse::<u64>()
        {
            return Ok(Expr::Number(Number::from(value)));
        }
    }
    if is_valid_float(raw)
        && let Ok(value) = raw.parse::<f64>()
        && value.is_finite()
        && let Some(number) = Number::from_f64(value)
    {
        return Ok(Expr::Number(number));
    }
    check_string(raw)?;
    Ok(Expr::String(raw.to_owned()))
}

fn measure_indent(line_no: u32, text: &str) -> Result<Option<(usize, &str)>, ConfigSourceError> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    let mut indent = 0usize;
    for byte in text.bytes() {
        match byte {
            b' ' => indent += 1,
            b'\t' => {
                return Err(parse_err(
                    line_no,
                    "tabs are not allowed in indentation; use spaces",
                ));
            }
            _ => break,
        }
    }
    let content = &text[indent..];
    if content.is_empty() || content.starts_with('#') {
        return Ok(None);
    }
    Ok(Some((indent, content)))
}

fn is_list_marker(content: &str) -> bool {
    content == "-"
        || content.starts_with("- ")
        || content
            .strip_prefix('-')
            .is_some_and(|rest| rest.trim_start_matches(' ').starts_with('#'))
}

fn is_kv_token(token: &Token) -> bool {
    match token {
        Token::Kv(_, _) => true,
        Token::Bare(s) => s.split_once('=').is_some_and(|(k, _)| !k.is_empty()),
        _ => false,
    }
}

fn token_as_key(token: &Token, line: u32) -> Result<String, ConfigSourceError> {
    match token {
        Token::Bare(s) | Token::Quoted(s) => {
            check_string(s)?;
            Ok(s.clone())
        }
        _ => Err(parse_err(line, "expected key name")),
    }
}

fn token_as_string(token: &Token, line: u32, what: &str) -> Result<String, ConfigSourceError> {
    match token {
        Token::Bare(s) | Token::Quoted(s) => {
            check_string(s)?;
            Ok(s.clone())
        }
        _ => Err(parse_err(line, format!("expected string for {what}"))),
    }
}

fn parse_http_status(text: &str) -> Option<u16> {
    if text.len() == 3 && text.bytes().all(|b| b.is_ascii_digit()) {
        let code = text.parse::<u16>().ok()?;
        if (100..=599).contains(&code) {
            return Some(code);
        }
    }
    None
}

fn collection_for_stanza(keyword: &str) -> Option<&'static str> {
    match keyword {
        "pool" | "upstream" => Some("upstream_pools"),
        "cert" | "certificate" => Some("certificates"),
        "tls" | "tls_profile" => Some("tls_profiles"),
        "service" | "http_service" => Some("http_services"),
        "listener" => Some("listeners"),
        "cache" | "cache_store" => Some("cache_stores"),
        "forward_proxy" => Some("forward_proxy_services"),
        "rtmp" => Some("rtmp_services"),
        "l4" => Some("l4_services"),
        _ => None,
    }
}

fn native_server_spec(keyword: &str) -> Option<(&'static str, bool)> {
    match keyword {
        "nginx_server" => Some(("nginx_server", false)),
        "haproxy_server" => Some(("haproxy_server", true)),
        "squid_server" => Some(("squid_server", false)),
        "apache_server" => Some(("apache_server", false)),
        "varnish_server" => Some(("varnish_server", false)),
        _ => None,
    }
}

fn merge_route_action(
    action_key: &str,
    existing: Expr,
    overlay: Expr,
    line: u32,
) -> Result<Expr, ConfigSourceError> {
    let Expr::Object(overlay_entries) = overlay else {
        return Err(parse_err(
            line,
            format!("duplicate key `{action_key}` in `route` block"),
        ));
    };
    let mut base_entries = match existing {
        Expr::Object(entries) => entries,
        other if action_key == "static" => vec![("root".to_owned(), other, line)],
        other if action_key == "proxy" => vec![("target".to_owned(), other, line)],
        _ => {
            return Err(parse_err(
                line,
                format!("duplicate key `{action_key}` in `route` block"),
            ));
        }
    };
    let mut seen: HashSet<String> = base_entries.iter().map(|(k, _, _)| k.clone()).collect();
    for (k, v, l) in overlay_entries {
        if !seen.insert(k.clone()) {
            return Err(parse_err(
                l,
                format!("duplicate key `{k}` in `route` `{action_key}` action"),
            ));
        }
        base_entries.push((k, v, l));
    }
    Ok(Expr::Object(base_entries))
}

fn tokenize_line(text: &str, line: u32, depth: usize) -> Result<Vec<Token>, ConfigSourceError> {
    tokenize_slice(text, line, depth, false)
}

fn tokenize_slice(
    text: &str,
    line: u32,
    depth: usize,
    in_array: bool,
) -> Result<Vec<Token>, ConfigSourceError> {
    if depth > MAX_STRUCTURAL_DEPTH {
        return Err(ConfigSourceError::StructuralDepth);
    }
    let bytes = text.as_bytes();
    let mut pos = 0usize;
    let mut tokens = Vec::new();

    while pos < bytes.len() {
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos >= bytes.len() || (!in_array && bytes[pos] == b'#') {
            break;
        }
        let (tok, next_pos) = scan_one_token(text, pos, line, depth, in_array)?;
        tokens.push(tok);
        pos = next_pos;
    }
    Ok(tokens)
}

fn scan_one_token(
    text: &str,
    start: usize,
    line: u32,
    depth: usize,
    in_array: bool,
) -> Result<(Token, usize), ConfigSourceError> {
    let bytes = text.as_bytes();
    let rest = &text[start..];

    if bytes[start] == b'"' {
        let (val, end) = parse_double_quoted(text, start, line)?;
        ensure_token_boundary(bytes, end, line, in_array, "closing `\"`")?;
        return Ok((Token::Quoted(val), end));
    }
    if bytes[start] == b'\'' {
        let (val, end) = parse_single_quoted(text, start, line)?;
        ensure_token_boundary(bytes, end, line, in_array, "closing `'`")?;
        return Ok((Token::Quoted(val), end));
    }
    if rest.starts_with("lua(") {
        let (expr, end) = parse_lua_expr(text, start, line)?;
        ensure_token_boundary(bytes, end, line, in_array, "`lua(...)`")?;
        return Ok((Token::LuaExpr(expr), end));
    }
    if bytes[start] == b'[' {
        let Some(close) = find_matching_bracket(text, start, line)? else {
            return Err(parse_err(line, "unterminated `[` inline array"));
        };
        let after = close + 1;
        if is_token_boundary(bytes, after, in_array) {
            let items = tokenize_slice(&text[start + 1..close], line, depth + 1, true)?;
            return Ok((Token::InlineArray(items), after));
        }
    }
    if rest.starts_with("{}") && is_token_boundary(bytes, start + 2, in_array) {
        return Ok((Token::EmptyObject, start + 2));
    }
    scan_bare_or_kv(text, start, line, depth, in_array)
}

fn scan_bare_or_kv(
    text: &str,
    start: usize,
    line: u32,
    depth: usize,
    in_array: bool,
) -> Result<(Token, usize), ConfigSourceError> {
    let bytes = text.as_bytes();
    let mut end = start;
    while end < bytes.len() && !bytes[end].is_ascii_whitespace() {
        if in_array && bytes[end] == b']' {
            break;
        }
        if bytes[end] == b'=' && end > start {
            let after_eq = end + 1;
            if after_eq < bytes.len()
                && (matches!(bytes[after_eq], b'"' | b'\'' | b'[')
                    || text[after_eq..].starts_with("lua("))
            {
                let key = text[start..end].to_owned();
                let (val_tok, val_end) = scan_one_token(text, after_eq, line, depth + 1, in_array)?;
                return Ok((Token::Kv(key, Box::new(val_tok)), val_end));
            }
        }
        end += 1;
    }
    let slice = &text[start..end];
    check_string(slice)?;
    Ok((Token::Bare(slice.to_owned()), end))
}

fn is_token_boundary(bytes: &[u8], pos: usize, in_array: bool) -> bool {
    pos >= bytes.len() || bytes[pos].is_ascii_whitespace() || (in_array && bytes[pos] == b']')
}

fn ensure_token_boundary(
    bytes: &[u8],
    pos: usize,
    line: u32,
    in_array: bool,
    after: &str,
) -> Result<(), ConfigSourceError> {
    if !is_token_boundary(bytes, pos, in_array) {
        return Err(parse_err(
            line,
            format!("unexpected trailing characters after {after}"),
        ));
    }
    Ok(())
}

/// Parses a JSON-compatible double-quoted string supporting `\"`, `\\`, `\/`, `\b`, `\f`, `\n`,
/// `\r`, `\t`, and `\uXXXX` (including UTF-16 surrogate pairs).
fn parse_double_quoted(
    text: &str,
    start: usize,
    line: u32,
) -> Result<(String, usize), ConfigSourceError> {
    let mut out = String::new();
    let mut chars = text[start + 1..].char_indices().peekable();
    while let Some((offset, ch)) = chars.next() {
        match ch {
            '"' => {
                check_string(&out)?;
                return Ok((out, start + 1 + offset + 1));
            }
            '\\' => {
                let Some((_, esc)) = chars.next() else {
                    return Err(parse_err(
                        line,
                        "unterminated escape sequence in double-quoted string",
                    ));
                };
                match esc {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'b' => out.push('\u{0008}'),
                    'f' => out.push('\u{000C}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => out.push(read_unicode_escape(&mut chars, line)?),
                    other => {
                        return Err(parse_err(
                            line,
                            format!("unsupported escape `\\{other}` in double-quoted string"),
                        ));
                    }
                }
            }
            other => out.push(other),
        }
    }
    Err(parse_err(line, "unterminated double-quoted string"))
}

fn read_unicode_escape(
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
    line: u32,
) -> Result<char, ConfigSourceError> {
    let code = read_hex4(chars, line)?;
    if (0xD800..=0xDBFF).contains(&code) {
        let (Some((_, '\\')), Some((_, 'u'))) = (chars.next(), chars.next()) else {
            return Err(parse_err(
                line,
                "unpaired UTF-16 high surrogate in double-quoted string",
            ));
        };
        let low = read_hex4(chars, line)?;
        if !(0xDC00..=0xDFFF).contains(&low) {
            return Err(parse_err(
                line,
                "invalid UTF-16 low surrogate in double-quoted string",
            ));
        }
        let scalar = 0x1_0000 + ((u32::from(code) - 0xD800) << 10) + (u32::from(low) - 0xDC00);
        char::from_u32(scalar).ok_or_else(|| parse_err(line, "invalid Unicode surrogate pair"))
    } else {
        char::from_u32(u32::from(code))
            .ok_or_else(|| parse_err(line, format!("invalid Unicode escape `\\u{code:04X}`")))
    }
}

fn read_hex4(
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
    line: u32,
) -> Result<u16, ConfigSourceError> {
    let mut value = 0_u16;
    for _ in 0..4 {
        let Some((_, ch)) = chars.next() else {
            return Err(parse_err(line, "incomplete `\\uXXXX` escape"));
        };
        let Some(digit) = ch.to_digit(16) else {
            return Err(parse_err(
                line,
                format!("invalid hex digit `{ch}` in `\\uXXXX` escape"),
            ));
        };
        value = (value << 4) | u16::try_from(digit).unwrap_or(0);
    }
    Ok(value)
}

/// Parses a single-quoted raw string (`'...'`), where `''` represents a literal single quote.
fn parse_single_quoted(
    text: &str,
    start: usize,
    line: u32,
) -> Result<(String, usize), ConfigSourceError> {
    let mut out = String::new();
    let mut chars = text[start + 1..].char_indices().peekable();
    while let Some((offset, ch)) = chars.next() {
        if ch == '\'' {
            if let Some(&(_, '\'')) = chars.peek() {
                chars.next();
                out.push('\'');
            } else {
                check_string(&out)?;
                return Ok((out, start + 1 + offset + 1));
            }
        } else {
            out.push(ch);
        }
    }
    Err(parse_err(line, "unterminated single-quoted string"))
}

fn parse_lua_expr(
    text: &str,
    start: usize,
    line: u32,
) -> Result<(String, usize), ConfigSourceError> {
    let bytes = text.as_bytes();
    let mut pos = start + 4;
    let mut depth = 1usize;

    while pos < bytes.len() {
        match bytes[pos] {
            b'"' | b'\'' => {
                let quote = bytes[pos];
                pos += 1;
                while pos < bytes.len() {
                    if bytes[pos] == b'\\' {
                        pos = (pos + 2).min(bytes.len());
                    } else if bytes[pos] == quote {
                        pos += 1;
                        break;
                    } else {
                        pos += 1;
                    }
                }
            }
            b'(' => {
                depth += 1;
                pos += 1;
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    let expr = text[start + 4..pos].trim();
                    if expr.is_empty() {
                        return Err(parse_err(line, "empty `lua(...)` expression"));
                    }
                    return Ok((expr.to_owned(), pos + 1));
                }
                pos += 1;
            }
            _ => pos += 1,
        }
    }
    Err(parse_err(line, "unterminated `lua(...)` expression"))
}

fn find_matching_bracket(
    text: &str,
    start: usize,
    line: u32,
) -> Result<Option<usize>, ConfigSourceError> {
    let bytes = text.as_bytes();
    let mut pos = start + 1;
    let mut depth = 1usize;

    while pos < bytes.len() {
        if bytes[pos] == b'"' {
            pos = parse_double_quoted(text, pos, line)?.1;
            continue;
        }
        if bytes[pos] == b'\'' {
            pos = parse_single_quoted(text, pos, line)?.1;
            continue;
        }
        if text[pos..].starts_with("lua(") {
            pos = parse_lua_expr(text, pos, line)?.1;
            continue;
        }
        match bytes[pos] {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(Some(pos));
                }
            }
            _ => {}
        }
        pos += 1;
    }
    Ok(None)
}
