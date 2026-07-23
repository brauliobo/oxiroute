use crate::{
    ByteRange, Diagnostic, DiagnosticStage, E_SOURCE_LIMIT, MAX_SOURCE_BYTES,
    MAX_TOKENS_PER_SOURCE, Report, Severity, SourceFile, SourceId, Span,
};

use super::E_SYNTAX;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Token {
    kind: TokenKind,
    span: Span,
}

impl Token {
    #[must_use]
    pub const fn kind(&self) -> &TokenKind {
        &self.kind
    }

    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TokenKind {
    /// Escape-normalized bytes. The raw lexeme remains available through the token span.
    Word(Vec<u8>),
    Semicolon,
    OpenBrace,
    CloseBrace,
}

#[must_use]
pub fn lex(source: &SourceFile) -> Report<Vec<Token>> {
    lex_with_limits(source, MAX_SOURCE_BYTES, MAX_TOKENS_PER_SOURCE)
}

pub(super) fn lex_with_limits(
    source: &SourceFile,
    max_source_bytes: usize,
    max_tokens: usize,
) -> Report<Vec<Token>> {
    let bounded_len = source.len().min(max_source_bytes);
    let source_was_truncated = bounded_len < source.len();
    let mut diagnostics = Vec::new();

    if source_was_truncated {
        diagnostics.push(
            Diagnostic::new(
                E_SOURCE_LIMIT,
                Severity::Error,
                DiagnosticStage::Source,
                format!("source exceeds the maximum size of {max_source_bytes} bytes"),
            )
            .with_primary_span(Span::new(
                source.id(),
                ByteRange::new(bounded_len, source.len()),
            )),
        );
    }

    let bytes = &source.bytes()[..bounded_len];

    Lexer::new(
        bytes,
        source.id(),
        max_tokens,
        source_was_truncated,
        diagnostics,
    )
    .run()
}

struct Lexer<'a> {
    bytes: &'a [u8],
    source: SourceId,
    max_tokens: usize,
    source_was_truncated: bool,
    index: usize,
    tokens: Vec<Token>,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Lexer<'a> {
    fn new(
        bytes: &'a [u8],
        source: SourceId,
        max_tokens: usize,
        source_was_truncated: bool,
        diagnostics: Vec<Diagnostic>,
    ) -> Self {
        Self {
            bytes,
            source,
            max_tokens,
            source_was_truncated,
            index: 0,
            tokens: Vec::new(),
            diagnostics,
        }
    }

    fn run(mut self) -> Report<Vec<Token>> {
        while let Some(byte) = self.current() {
            match byte {
                byte if is_whitespace(byte) => self.index += 1,
                b'#' => self.skip_comment(),
                _ if self.tokens.len() == self.max_tokens => {
                    self.token_limit();
                    break;
                }
                b';' => self.push_punctuation(TokenKind::Semicolon),
                b'{' => self.push_punctuation(TokenKind::OpenBrace),
                b'}' => self.push_punctuation(TokenKind::CloseBrace),
                b'\'' | b'"' => self.quoted_word(byte),
                _ => self.unquoted_word(),
            }
        }

        Report::new(self.tokens, self.diagnostics)
    }

    fn current(&self) -> Option<u8> {
        self.bytes.get(self.index).copied()
    }

    fn skip_comment(&mut self) {
        while self.current().is_some_and(|byte| byte != b'\n') {
            self.index += 1;
        }
    }

    fn push_punctuation(&mut self, kind: TokenKind) {
        let start = self.index;
        self.index += 1;
        self.tokens.push(Token {
            kind,
            span: self.span(start, self.index),
        });
    }

    fn unquoted_word(&mut self) {
        let start = self.index;
        let mut raw = Vec::new();
        let mut variable = false;
        let mut trailing_escape = None;

        while let Some(byte) = self.current() {
            if byte == b'{' && variable {
                raw.push(byte);
                self.index += 1;
                continue;
            }

            variable = false;
            match byte {
                byte if is_whitespace(byte) || matches!(byte, b';' | b'{') => break,
                b'\\' => {
                    let escape_start = self.index;
                    raw.push(byte);
                    self.index += 1;
                    if let Some(escaped) = self.current() {
                        raw.push(escaped);
                        self.index += 1;
                    } else {
                        trailing_escape = Some(escape_start);
                        break;
                    }
                }
                b'$' => {
                    variable = true;
                    raw.push(byte);
                    self.index += 1;
                }
                _ => {
                    raw.push(byte);
                    self.index += 1;
                }
            }
        }

        if let Some(escape_start) = trailing_escape {
            if self.source_was_truncated {
                return;
            }
            self.syntax(
                "unterminated escape in nginx word",
                escape_start,
                self.bytes.len(),
            );
        }

        if self.source_was_truncated && self.index == self.bytes.len() {
            return;
        }

        self.tokens.push(Token {
            kind: TokenKind::Word(decode_word(&raw)),
            span: self.span(start, self.index),
        });
    }

    fn quoted_word(&mut self, quote: u8) {
        let start = self.index;
        self.index += 1;
        let mut raw = Vec::new();

        while let Some(byte) = self.current() {
            match byte {
                b'\\' => {
                    let escape_start = self.index;
                    raw.push(byte);
                    self.index += 1;
                    if let Some(escaped) = self.current() {
                        raw.push(escaped);
                        self.index += 1;
                    } else {
                        if self.source_was_truncated {
                            return;
                        }
                        self.syntax(
                            "unterminated escape in quoted nginx word",
                            escape_start,
                            self.bytes.len(),
                        );
                        return;
                    }
                }
                byte if byte == quote => {
                    self.index += 1;
                    self.tokens.push(Token {
                        kind: TokenKind::Word(decode_word(&raw)),
                        span: self.span(start, self.index),
                    });
                    self.require_quoted_boundary();
                    return;
                }
                _ => {
                    raw.push(byte);
                    self.index += 1;
                }
            }
        }

        if self.source_was_truncated {
            return;
        }
        self.syntax("unterminated quoted nginx word", start, self.bytes.len());
    }

    fn require_quoted_boundary(&mut self) {
        let Some(byte) = self.current() else {
            return;
        };
        if is_whitespace(byte) || matches!(byte, b';' | b'{' | b')') {
            return;
        }

        let end = self.index + 1;
        self.syntax(
            "quoted nginx word must be followed by whitespace, `;`, `{`, or `)`",
            self.index,
            end,
        );

        while let Some(byte) = self.current() {
            if is_whitespace(byte) || matches!(byte, b';' | b'{' | b'}') {
                break;
            }
            self.index += 1;
        }
    }

    fn syntax(&mut self, message: &'static str, start: usize, end: usize) {
        self.diagnostics.push(
            Diagnostic::new(E_SYNTAX, Severity::Error, DiagnosticStage::Lex, message)
                .with_primary_span(self.span(start, end)),
        );
    }

    fn token_limit(&mut self) {
        let start = self.index;
        let end = start + 1;
        self.diagnostics.push(
            Diagnostic::new(
                E_SOURCE_LIMIT,
                Severity::Error,
                DiagnosticStage::Lex,
                format!(
                    "nginx token count exceeds the maximum of {} per source",
                    self.max_tokens
                ),
            )
            .with_primary_span(self.span(start, end)),
        );
    }

    fn span(&self, start: usize, end: usize) -> Span {
        Span::new(self.source, ByteRange::new(start, end))
    }
}

fn decode_word(raw: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(raw.len());
    let mut index = 0;

    while let Some(&byte) = raw.get(index) {
        if byte != b'\\' {
            decoded.push(byte);
            index += 1;
            continue;
        }

        let Some(&escaped) = raw.get(index + 1) else {
            decoded.push(byte);
            break;
        };
        match escaped {
            b'"' | b'\'' | b'\\' => decoded.push(escaped),
            b't' => decoded.push(b'\t'),
            b'r' => decoded.push(b'\r'),
            b'n' => decoded.push(b'\n'),
            _ => {
                decoded.push(byte);
                decoded.push(escaped);
            }
        }
        index += 2;
    }

    decoded
}

const fn is_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

#[cfg(test)]
mod tests {
    use crate::{ByteRange, DiagnosticStage, E_SOURCE_LIMIT, SourceFile, SourceId};

    use super::lex_with_limits;

    #[test]
    fn token_limit_keeps_a_deterministic_bounded_prefix() {
        let boundary = SourceFile::new(SourceId::new(1), "boundary.conf", b";;;".as_slice());
        let boundary_report = lex_with_limits(&boundary, usize::MAX, 3);
        assert_eq!(boundary_report.value().len(), 3);
        assert!(boundary_report.diagnostics().is_empty());

        let over = SourceFile::new(SourceId::new(1), "over.conf", b";;;;".as_slice());
        let first = lex_with_limits(&over, usize::MAX, 3);
        let second = lex_with_limits(&over, usize::MAX, 3);

        assert_eq!(first, second);
        assert_eq!(first.value().len(), 3);
        assert_eq!(first.diagnostics().len(), 1);
        assert_eq!(first.diagnostics()[0].code(), E_SOURCE_LIMIT);
        assert_eq!(first.diagnostics()[0].stage(), DiagnosticStage::Lex);
        assert_eq!(
            first.diagnostics()[0]
                .primary_span()
                .expect("located token limit")
                .range(),
            ByteRange::new(3, 4)
        );
    }
}
