use crate::{
    ByteRange, Diagnostic, DiagnosticStage, E_SOURCE_LIMIT, Report, Severity, SourceFile, Span,
};

use super::E_VCL_SYNTAX;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum TokenKind {
    Word(Vec<u8>),
    String(Vec<u8>),
    Number(Vec<u8>),
    InlineC,
    OpenBrace,
    CloseBrace,
    OpenParen,
    CloseParen,
    Semicolon,
    Comma,
    Assign,
    AddAssign,
    SubtractAssign,
    MultiplyAssign,
    DivideAssign,
    Equal,
    NotEqual,
    Match,
    NotMatch,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    And,
    Or,
    Not,
    Plus,
    Minus,
    Star,
    Slash,
}

pub(super) fn lex(
    source: &SourceFile,
    max_source_bytes: usize,
    max_tokens: usize,
) -> Report<Vec<Token>> {
    let bytes = source.bytes();
    let end = bytes.len().min(max_source_bytes);
    let mut lexer = Lexer {
        source,
        bytes: &bytes[..end],
        offset: 0,
        max_tokens,
        tokens: Vec::new(),
        diagnostics: Vec::new(),
    };

    if bytes.len() > max_source_bytes {
        lexer.diagnostics.push(
            Diagnostic::new(
                E_SOURCE_LIMIT,
                Severity::Error,
                DiagnosticStage::Source,
                format!("VCL source exceeds the maximum of {max_source_bytes} bytes"),
            )
            .with_primary_span(lexer.span(end, end.saturating_add(1).min(bytes.len()))),
        );
    }

    lexer.run();
    Report::new(lexer.tokens, lexer.diagnostics)
}

struct Lexer<'a> {
    source: &'a SourceFile,
    bytes: &'a [u8],
    offset: usize,
    max_tokens: usize,
    tokens: Vec<Token>,
    diagnostics: Vec<Diagnostic>,
}

impl Lexer<'_> {
    fn run(&mut self) {
        while self.offset < self.bytes.len() {
            self.skip_trivia();
            if self.offset == self.bytes.len() {
                break;
            }
            if self.tokens.len() == self.max_tokens {
                self.diagnostics.push(
                    Diagnostic::new(
                        E_SOURCE_LIMIT,
                        Severity::Error,
                        DiagnosticStage::Lex,
                        format!(
                            "VCL token count exceeds the maximum of {} per source",
                            self.max_tokens
                        ),
                    )
                    .with_primary_span(self.span(self.offset, self.offset + 1)),
                );
                break;
            }
            self.lex_token();
        }
    }

    fn skip_trivia(&mut self) {
        loop {
            while self
                .bytes
                .get(self.offset)
                .is_some_and(u8::is_ascii_whitespace)
            {
                self.offset += 1;
            }

            if self.bytes.get(self.offset) == Some(&b'#')
                || self.bytes.get(self.offset..self.offset + 2) == Some(b"//")
            {
                while self
                    .bytes
                    .get(self.offset)
                    .is_some_and(|byte| *byte != b'\n')
                {
                    self.offset += 1;
                }
                continue;
            }

            if self.bytes.get(self.offset..self.offset + 2) == Some(b"/*") {
                let start = self.offset;
                self.offset += 2;
                while self.offset + 1 < self.bytes.len()
                    && self.bytes.get(self.offset..self.offset + 2) != Some(b"*/")
                {
                    self.offset += 1;
                }
                if self.offset + 1 < self.bytes.len() {
                    self.offset += 2;
                } else {
                    self.offset = self.bytes.len();
                    self.syntax("unterminated block comment", start, self.offset);
                }
                continue;
            }
            break;
        }
    }

    fn lex_token(&mut self) {
        let start = self.offset;
        if self.bytes.get(start..start + 2) == Some(b"C{") {
            self.offset += 2;
            while self.offset + 1 < self.bytes.len()
                && self.bytes.get(self.offset..self.offset + 2) != Some(b"}C")
            {
                self.offset += 1;
            }
            if self.offset + 1 < self.bytes.len() {
                self.offset += 2;
            } else {
                self.offset = self.bytes.len();
                self.syntax("unterminated inline C block", start, self.offset);
            }
            self.push(TokenKind::InlineC, start);
            return;
        }

        if self.bytes[start] == b'"' {
            self.lex_string();
            return;
        }

        if self.bytes[start].is_ascii_digit() {
            self.offset += 1;
            while self
                .bytes
                .get(self.offset)
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'%'))
            {
                self.offset += 1;
            }
            self.push(
                TokenKind::Number(self.bytes[start..self.offset].to_vec()),
                start,
            );
            return;
        }

        if is_word_start(self.bytes[start]) {
            self.offset += 1;
            while self
                .bytes
                .get(self.offset)
                .is_some_and(|byte| is_word_continue(*byte))
            {
                self.offset += 1;
            }
            self.push(
                TokenKind::Word(self.bytes[start..self.offset].to_vec()),
                start,
            );
            return;
        }

        let (kind, width) = match self.bytes.get(start..start + 2) {
            Some(b"+=") => (TokenKind::AddAssign, 2),
            Some(b"-=") => (TokenKind::SubtractAssign, 2),
            Some(b"*=") => (TokenKind::MultiplyAssign, 2),
            Some(b"/=") => (TokenKind::DivideAssign, 2),
            Some(b"==") => (TokenKind::Equal, 2),
            Some(b"!=") => (TokenKind::NotEqual, 2),
            Some(b"!~") => (TokenKind::NotMatch, 2),
            Some(b"<=") => (TokenKind::LessEqual, 2),
            Some(b">=") => (TokenKind::GreaterEqual, 2),
            Some(b"&&") => (TokenKind::And, 2),
            Some(b"||") => (TokenKind::Or, 2),
            _ => match self.bytes[start] {
                b'{' => (TokenKind::OpenBrace, 1),
                b'}' => (TokenKind::CloseBrace, 1),
                b'(' => (TokenKind::OpenParen, 1),
                b')' => (TokenKind::CloseParen, 1),
                b';' => (TokenKind::Semicolon, 1),
                b',' => (TokenKind::Comma, 1),
                b'=' => (TokenKind::Assign, 1),
                b'~' => (TokenKind::Match, 1),
                b'<' => (TokenKind::Less, 1),
                b'>' => (TokenKind::Greater, 1),
                b'!' => (TokenKind::Not, 1),
                b'+' => (TokenKind::Plus, 1),
                b'-' => (TokenKind::Minus, 1),
                b'*' => (TokenKind::Star, 1),
                b'/' => (TokenKind::Slash, 1),
                byte => {
                    self.offset += 1;
                    self.syntax(
                        &format!("unexpected byte 0x{byte:02x} in VCL source"),
                        start,
                        self.offset,
                    );
                    return;
                }
            },
        };
        self.offset += width;
        self.push(kind, start);
    }

    fn lex_string(&mut self) {
        let start = self.offset;
        self.offset += 1;
        let mut value = Vec::new();
        let mut terminated = false;
        while let Some(&byte) = self.bytes.get(self.offset) {
            self.offset += 1;
            match byte {
                b'"' => {
                    terminated = true;
                    break;
                }
                b'\\' => {
                    let Some(&escaped) = self.bytes.get(self.offset) else {
                        break;
                    };
                    self.offset += 1;
                    value.push(match escaped {
                        b'n' => b'\n',
                        b'r' => b'\r',
                        b't' => b'\t',
                        other => other,
                    });
                }
                other => value.push(other),
            }
        }
        if !terminated {
            self.syntax("unterminated VCL string", start, self.offset);
        }
        self.push(TokenKind::String(value), start);
    }

    fn push(&mut self, kind: TokenKind, start: usize) {
        self.tokens.push(Token {
            kind,
            span: self.span(start, self.offset),
        });
    }

    fn syntax(&mut self, message: &str, start: usize, end: usize) {
        self.diagnostics.push(
            Diagnostic::new(E_VCL_SYNTAX, Severity::Error, DiagnosticStage::Lex, message)
                .with_primary_span(self.span(start, end)),
        );
    }

    fn span(&self, start: usize, end: usize) -> Span {
        Span::new(self.source.id(), ByteRange::new(start, end))
    }
}

fn is_word_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'.')
}

fn is_word_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-' | b':')
}
