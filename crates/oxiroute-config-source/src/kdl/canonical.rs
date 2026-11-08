//! A single-pass reader for the canonical KDL text that [`super::render`] writes.
//!
//! The generic KDL parser keeps recoverable diagnostics state for every token, which costs thousands
//! of instructions per source byte. The daemon re-reads its own canonical output on every reload and
//! reconciliation, so that output is read here directly into the value tree. The reader accepts only
//! the exact shape the renderer emits (spaces, `\n` line ends, `(object)`/`(array)` containers,
//! bare or simply escaped names, integer/float/`#true`/`#false`/`#null`/quoted-string scalars) and
//! answers `None` for anything else, in which case the generic parser decides, including its
//! diagnostics. Every document accepted here decodes identically through the generic parser.

use serde_json::{Map, Number, Value};

use crate::limits::{MAX_NODES, MAX_STRING_BYTES, MAX_STRUCTURAL_DEPTH, validate_value};

/// Container nesting accepted here; deeper documents fall back to the generic bound checks.
const MAX_DEPTH: usize = MAX_STRUCTURAL_DEPTH / 2;

/// Reads `source` as canonical KDL, returning the validated value tree.
pub(super) fn decode(source: &str) -> Option<Value> {
    let mut reader = Reader {
        bytes: source.as_bytes(),
        source,
        at: 0,
        nodes: 0,
    };
    let root = reader.block(0, false)?;
    validate_value(&root).ok()?;
    Some(root)
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Shape {
    Scalar,
    Object,
    Array,
}

struct Reader<'a> {
    bytes: &'a [u8],
    source: &'a str,
    at: usize,
    nodes: usize,
}

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn eat(&mut self, expected: u8) -> Option<()> {
        (self.peek() == Some(expected)).then(|| self.at += 1)
    }

    fn skip_spaces(&mut self) -> usize {
        let start = self.at;
        while self.peek() == Some(b' ') {
            self.at += 1;
        }
        self.at - start
    }

    /// The rest of a line: optional spaces, then a newline or the end of the document.
    fn end_of_line(&mut self) -> Option<()> {
        self.skip_spaces();
        match self.peek() {
            None => Some(()),
            Some(b'\n') => {
                self.at += 1;
                Some(())
            }
            Some(_) => None,
        }
    }

    /// Nodes up to the closing brace (or the end of the document at the root).
    fn block(&mut self, depth: usize, array: bool) -> Option<Value> {
        if depth > MAX_DEPTH {
            return None;
        }
        let mut object = Map::new();
        let mut items = Vec::new();
        loop {
            self.skip_spaces();
            match self.peek() {
                None if depth == 0 => break,
                Some(b'\n') => {
                    self.at += 1;
                    continue;
                }
                Some(b'}') if depth > 0 => {
                    self.at += 1;
                    self.end_of_line()?;
                    break;
                }
                _ => {}
            }
            let shape = self.shape()?;
            let name = self.name()?;
            self.nodes += 1;
            if self.nodes > MAX_NODES || (array && name != "-") {
                return None;
            }
            if self.skip_spaces() == 0 {
                return None;
            }
            let value = self.node_value(shape, depth)?;
            if array {
                items.push(value);
            } else if object.insert(name, value).is_some() {
                return None;
            }
        }
        Some(if array {
            Value::Array(items)
        } else {
            Value::Object(object)
        })
    }

    fn node_value(&mut self, shape: Shape, depth: usize) -> Option<Value> {
        if shape == Shape::Scalar {
            let scalar = self.scalar()?;
            self.end_of_line()?;
            return Some(scalar);
        }
        self.eat(b'{')?;
        self.end_of_line()?;
        self.block(depth + 1, shape == Shape::Array)
    }

    /// The `(object)` or `(array)` annotation of a container node.
    fn shape(&mut self) -> Option<Shape> {
        if self.peek() != Some(b'(') {
            return Some(Shape::Scalar);
        }
        let rest = &self.bytes[self.at..];
        let (shape, length) = if rest.starts_with(b"(object)") {
            (Shape::Object, 8)
        } else if rest.starts_with(b"(array)") {
            (Shape::Array, 7)
        } else {
            return None;
        };
        self.at += length;
        Some(shape)
    }

    fn name(&mut self) -> Option<String> {
        if self.peek() == Some(b'"') {
            return self.string();
        }
        let start = self.at;
        while self.peek().is_some_and(is_name_byte) {
            self.at += 1;
        }
        let name = &self.source[start..self.at];
        is_bare_name(name).then(|| name.to_owned())
    }

    fn scalar(&mut self) -> Option<Value> {
        if self.peek() == Some(b'"') {
            return self.string().map(Value::String);
        }
        let start = self.at;
        while self
            .peek()
            .is_some_and(|byte| !matches!(byte, b' ' | b'\n'))
        {
            self.at += 1;
        }
        match &self.source[start..self.at] {
            "#true" => Some(Value::Bool(true)),
            "#false" => Some(Value::Bool(false)),
            "#null" => Some(Value::Null),
            token => number(token),
        }
    }

    /// A quoted string whose escapes are limited to those `serde_json` and the renderer share.
    fn string(&mut self) -> Option<String> {
        self.eat(b'"')?;
        let mut out = String::new();
        let mut run = self.at;
        loop {
            let byte = self.peek()?;
            match byte {
                b'"' => {
                    out.push_str(&self.source[run..self.at]);
                    self.at += 1;
                    let multi_line_start = out.is_empty() && self.peek() == Some(b'"');
                    return (!multi_line_start && out.len() <= MAX_STRING_BYTES).then_some(out);
                }
                b'\\' => {
                    out.push_str(&self.source[run..self.at]);
                    let escaped = match *self.bytes.get(self.at + 1)? {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        _ => return None,
                    };
                    out.push(escaped);
                    self.at += 2;
                    run = self.at;
                }
                0x20..=0x7e => self.at += 1,
                0x80.. => {
                    let character = self.source[self.at..].chars().next()?;
                    if is_disallowed_in_string(character) {
                        return None;
                    }
                    self.at += character.len_utf8();
                }
                _ => return None,
            }
        }
    }
}

/// Whether the renderer writes `name` as an unquoted identifier.
pub(super) fn is_bare_name(name: &str) -> bool {
    match name.as_bytes() {
        [] => false,
        b"-" => true,
        [first, rest @ ..] => {
            (first.is_ascii_alphabetic() || *first == b'_')
                && rest.iter().copied().all(is_name_byte)
                && !is_keyword(name)
        }
    }
}

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
}

fn is_keyword(name: &str) -> bool {
    matches!(name, "true" | "false" | "null" | "inf" | "nan")
}

fn is_disallowed_in_string(character: char) -> bool {
    matches!(character,
        '\u{85}' | '\u{2028}' | '\u{2029}'
        | '\u{200E}'..='\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2066}'..='\u{2069}'
        | '\u{FEFF}'
    )
}

/// `-?digits`, or `-?digits[.digits][e[+-]digits]` with at least a fraction or an exponent.
fn number(token: &str) -> Option<Value> {
    let digits = token.strip_prefix('-').unwrap_or(token);
    let integer_end = digits.bytes().take_while(u8::is_ascii_digit).count();
    if integer_end == 0 {
        return None;
    }
    if integer_end == digits.len() {
        return if let Ok(value) = token.parse::<i64>() {
            Some(Value::Number(value.into()))
        } else {
            token
                .parse::<u64>()
                .ok()
                .map(|value| Value::Number(value.into()))
        };
    }
    let mut rest = &digits[integer_end..];
    if let Some(fraction) = rest.strip_prefix('.') {
        let length = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if length == 0 {
            return None;
        }
        rest = &fraction[length..];
    }
    if let Some(exponent) = rest.strip_prefix(['e', 'E']) {
        let exponent = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
        if exponent.is_empty() || !exponent.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        rest = "";
    }
    if !rest.is_empty() {
        return None;
    }
    Number::from_f64(token.parse().ok()?).map(Value::Number)
}
