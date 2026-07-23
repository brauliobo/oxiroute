use crate::{
    ByteRange, Diagnostic, DiagnosticStage, E_SOURCE_LIMIT, MAX_SOURCE_BYTES,
    MAX_TOKENS_PER_SOURCE, Report, Severity, SourceFile, SourceId, Span,
};

use super::{E_SYNTAX, MAX_WORDS_PER_LINE};

/// One `HAProxy` word after top-level quote removal and documented escape decoding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Word {
    pub value: Vec<u8>,
    /// The raw word lexeme, including any quotes and escapes.
    pub span: Span,
    /// Unescaped environment references found in weakly quoted segments.
    pub environment_references: Vec<Span>,
}

impl Word {
    #[must_use]
    pub const fn has_environment_references(&self) -> bool {
        !self.environment_references.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineEnding {
    None,
    Lf,
    CrLf,
}

/// One LF-delimited `HAProxy` source record. Its span includes the original line ending.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Line {
    pub words: Vec<Word>,
    /// The unprotected `#` through the last byte before the line ending.
    pub comment: Option<Span>,
    pub ending: LineEnding,
    pub span: Span,
}

#[must_use]
pub fn lex(source: &SourceFile) -> Report<Vec<Line>> {
    lex_with_limits(source, MAX_SOURCE_BYTES, MAX_TOKENS_PER_SOURCE)
}

pub(super) fn lex_with_limits(
    source: &SourceFile,
    max_source_bytes: usize,
    max_tokens: usize,
) -> Report<Vec<Line>> {
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

    Lexer {
        bytes: &source.bytes()[..bounded_len],
        source: source.id(),
        source_was_truncated,
        max_tokens,
        token_count: 0,
        index: 0,
        lines: Vec::new(),
        diagnostics,
    }
    .run()
}

struct Lexer<'a> {
    bytes: &'a [u8],
    source: SourceId,
    source_was_truncated: bool,
    max_tokens: usize,
    token_count: usize,
    index: usize,
    lines: Vec<Line>,
    diagnostics: Vec<Diagnostic>,
}

impl Lexer<'_> {
    fn run(mut self) -> Report<Vec<Line>> {
        while self.index < self.bytes.len() {
            let line_start = self.index;
            let record_end = self.find_record_end();
            let terminated = record_end < self.bytes.len();
            let line_end = record_end + usize::from(terminated);
            let content_end = self.find_content_end(record_end);
            let ending = if !terminated {
                LineEnding::None
            } else if content_end + 1 == record_end && self.bytes[content_end] == b'\r' {
                LineEnding::CrLf
            } else {
                LineEnding::Lf
            };

            if self.source_was_truncated && !terminated {
                break;
            }

            if !terminated {
                self.syntax(
                    "final HAProxy line is not terminated by LF",
                    line_start,
                    line_end,
                );
            }

            if let Some(nul_offset) = self.bytes[line_start..record_end]
                .iter()
                .position(|byte| *byte == b'\0')
                .map(|offset| line_start + offset)
            {
                self.syntax(
                    "NUL byte is not permitted in HAProxy source",
                    nul_offset,
                    nul_offset + 1,
                );
                break;
            }

            match self.lex_line(line_start, content_end, ending, line_end) {
                LineResult::Line(line) => self.lines.push(line),
                LineResult::Invalid => {}
                LineResult::TokenLimit { offset } => {
                    self.diagnostics.push(
                        Diagnostic::new(
                            E_SOURCE_LIMIT,
                            Severity::Error,
                            DiagnosticStage::Lex,
                            format!(
                                "HAProxy token count exceeds the maximum of {} per source",
                                self.max_tokens
                            ),
                        )
                        .with_primary_span(self.span(offset, offset + 1)),
                    );
                    break;
                }
            }

            self.index = line_end;
        }

        Report::new(self.lines, self.diagnostics)
    }

    fn find_record_end(&self) -> usize {
        let mut end = self.index;
        while self.bytes.get(end).is_some_and(|byte| *byte != b'\n') {
            end += 1;
        }
        end
    }

    fn find_content_end(&self, record_end: usize) -> usize {
        self.bytes[self.index..record_end]
            .iter()
            .position(|byte| *byte == b'\r')
            .map_or(record_end, |offset| self.index + offset)
    }

    fn lex_line(
        &mut self,
        line_start: usize,
        content_end: usize,
        ending: LineEnding,
        line_end: usize,
    ) -> LineResult {
        let mut cursor = line_start;
        let mut words = Vec::new();
        let mut comment = None;

        while cursor < content_end {
            while self
                .bytes
                .get(cursor)
                .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
            {
                cursor += 1;
            }
            if cursor == content_end {
                break;
            }
            if self.bytes[cursor] == b'#' {
                comment = Some(self.span(cursor, content_end));
                break;
            }
            if words.len() == MAX_WORDS_PER_LINE {
                self.diagnostics.push(
                    Diagnostic::new(
                        E_SOURCE_LIMIT,
                        Severity::Error,
                        DiagnosticStage::Lex,
                        format!(
                            "HAProxy line exceeds the native maximum of {MAX_WORDS_PER_LINE} words"
                        ),
                    )
                    .with_primary_span(self.span(cursor, cursor + 1)),
                );
                return LineResult::Invalid;
            }
            if self.token_count == self.max_tokens {
                return LineResult::TokenLimit { offset: cursor };
            }

            match self.lex_word(cursor, content_end) {
                Ok((word, next, starts_comment)) => {
                    self.token_count += 1;
                    words.push(word);
                    cursor = next;
                    if starts_comment {
                        comment = Some(self.span(cursor, content_end));
                        break;
                    }
                }
                Err(()) => return LineResult::Invalid,
            }
        }

        LineResult::Line(Line {
            words,
            comment,
            ending,
            span: self.span(line_start, line_end),
        })
    }

    fn lex_word(&mut self, start: usize, content_end: usize) -> Result<(Word, usize, bool), ()> {
        let mut cursor = start;
        let mut value = Vec::new();
        let mut environment_references = Vec::new();
        let mut quote = None;
        let mut saw_quote = false;

        while cursor < content_end {
            let byte = self.bytes[cursor];
            match quote {
                Some(Quote::Strong { .. }) => {
                    if byte == b'\'' {
                        quote = None;
                    } else {
                        value.push(byte);
                    }
                    cursor += 1;
                }
                Some(Quote::Weak { .. }) => match byte {
                    b'"' => {
                        quote = None;
                        cursor += 1;
                    }
                    b'\\' => self.decode_escape(&mut cursor, content_end, true, &mut value)?,
                    b'$' => {
                        let reference_end = self.environment_reference_end(cursor, content_end)?;
                        value.extend_from_slice(&self.bytes[cursor..reference_end]);
                        environment_references.push(self.span(cursor, reference_end));
                        cursor = reference_end;
                    }
                    _ => {
                        value.push(byte);
                        cursor += 1;
                    }
                },
                None => match byte {
                    b' ' | b'\t' => break,
                    b'#' => {
                        if saw_quote && value.is_empty() {
                            self.syntax(
                                "empty quoted argument is not permitted in HAProxy source",
                                start,
                                cursor,
                            );
                            return Err(());
                        }
                        return Ok((
                            Word {
                                value,
                                span: self.span(start, cursor),
                                environment_references,
                            },
                            cursor,
                            true,
                        ));
                    }
                    b'\'' => {
                        saw_quote = true;
                        quote = Some(Quote::Strong { start: cursor });
                        cursor += 1;
                    }
                    b'"' => {
                        saw_quote = true;
                        quote = Some(Quote::Weak { start: cursor });
                        cursor += 1;
                    }
                    b'\\' => self.decode_escape(&mut cursor, content_end, false, &mut value)?,
                    _ => {
                        value.push(byte);
                        cursor += 1;
                    }
                },
            }
        }

        if let Some(quote) = quote {
            let quote_start = quote.start();
            let description = match quote {
                Quote::Strong { .. } => "unterminated strong quote in HAProxy word",
                Quote::Weak { .. } => "unterminated weak quote in HAProxy word",
            };
            self.syntax(description, quote_start, content_end);
            return Err(());
        }
        if saw_quote && value.is_empty() {
            self.syntax(
                "empty quoted argument is not permitted in HAProxy source",
                start,
                cursor,
            );
            return Err(());
        }

        Ok((
            Word {
                value,
                span: self.span(start, cursor),
                environment_references,
            },
            cursor,
            false,
        ))
    }

    fn decode_escape(
        &mut self,
        cursor: &mut usize,
        content_end: usize,
        weakly_quoted: bool,
        value: &mut Vec<u8>,
    ) -> Result<(), ()> {
        let escape_start = *cursor;
        let Some(&escaped) = self
            .bytes
            .get(escape_start + 1)
            .filter(|_| escape_start + 1 < content_end)
        else {
            value.push(b'\\');
            *cursor += 1;
            return Ok(());
        };

        match escaped {
            b' ' | b'\t' | b'#' | b'\\' | b'\'' | b'"' => {
                value.push(escaped);
                *cursor += 2;
            }
            b'$' if weakly_quoted => {
                value.push(b'$');
                *cursor += 2;
            }
            b'n' => {
                value.push(b'\n');
                *cursor += 2;
            }
            b'r' => {
                value.push(b'\r');
                *cursor += 2;
            }
            b't' => {
                value.push(b'\t');
                *cursor += 2;
            }
            b'x' => {
                let end = (escape_start + 4).min(content_end);
                let Some(hex) = self.bytes.get(escape_start + 2..escape_start + 4) else {
                    self.syntax(
                        "truncated hexadecimal escape in HAProxy word",
                        escape_start,
                        end,
                    );
                    return Err(());
                };
                let Some(high) = hex_value(hex[0]) else {
                    self.syntax(
                        "invalid hexadecimal escape in HAProxy word",
                        escape_start,
                        end,
                    );
                    return Err(());
                };
                let Some(low) = hex_value(hex[1]) else {
                    self.syntax(
                        "invalid hexadecimal escape in HAProxy word",
                        escape_start,
                        end,
                    );
                    return Err(());
                };
                let decoded = (high << 4) | low;
                if decoded == b'\0' {
                    self.syntax(
                        "NUL-producing escape is not permitted in HAProxy source",
                        escape_start,
                        end,
                    );
                    return Err(());
                }
                value.push(decoded);
                *cursor += 4;
            }
            _ => {
                value.push(b'\\');
                *cursor += 1;
            }
        }
        Ok(())
    }

    fn environment_reference_end(&mut self, start: usize, content_end: usize) -> Result<usize, ()> {
        let mut cursor = start + 1;
        let braced = self.bytes.get(cursor) == Some(&b'{');
        if braced {
            cursor += 1;
        }

        let name_start = cursor;
        if !self
            .bytes
            .get(cursor)
            .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'.'))
        {
            let end = (cursor + 1).min(content_end).max(start + 1);
            self.syntax(
                "invalid environment reference in weakly quoted HAProxy word",
                start,
                end,
            );
            return Err(());
        }
        cursor += 1;
        while self
            .bytes
            .get(cursor)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            cursor += 1;
        }
        debug_assert!(cursor > name_start);

        if self.bytes[name_start] == b'.'
            && !matches!(
                &self.bytes[name_start..cursor],
                b".LINE" | b".FILE" | b".SECTION"
            )
        {
            self.syntax(
                "unsupported built-in HAProxy environment reference",
                start,
                cursor,
            );
            return Err(());
        }

        if !braced {
            return Ok(cursor);
        }

        if self.bytes.get(cursor) == Some(&b'[') {
            if self.bytes.get(cursor..cursor + 3) != Some(b"[*]".as_slice()) {
                let end = (cursor + 3).min(content_end);
                self.syntax(
                    "invalid word-expansion suffix in HAProxy environment reference",
                    cursor,
                    end,
                );
                return Err(());
            }
            cursor += 3;
        }
        if self.bytes.get(cursor) == Some(&b'-') {
            cursor += 1;
            while cursor < content_end && self.bytes[cursor] != b'}' {
                cursor += 1;
            }
        }
        if self.bytes.get(cursor) != Some(&b'}') {
            self.syntax(
                "unterminated braced environment reference in HAProxy word",
                start,
                content_end,
            );
            return Err(());
        }
        Ok(cursor + 1)
    }

    fn syntax(&mut self, message: &'static str, start: usize, end: usize) {
        self.diagnostics.push(
            Diagnostic::new(E_SYNTAX, Severity::Error, DiagnosticStage::Lex, message)
                .with_primary_span(self.span(start, end)),
        );
    }

    const fn span(&self, start: usize, end: usize) -> Span {
        Span::new(self.source, ByteRange::new(start, end))
    }
}

enum LineResult {
    Line(Line),
    Invalid,
    TokenLimit { offset: usize },
}

#[derive(Clone, Copy)]
enum Quote {
    Strong { start: usize },
    Weak { start: usize },
}

impl Quote {
    const fn start(self) -> usize {
        match self {
            Self::Strong { start } | Self::Weak { start } => start,
        }
    }
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::{ByteRange, DiagnosticStage, E_SOURCE_LIMIT, SourceFile, SourceId};

    use super::lex_with_limits;

    #[test]
    fn token_limit_drops_the_incomplete_line_and_keeps_a_bounded_prefix() {
        let source = SourceFile::new(
            SourceId::new(1),
            "haproxy.cfg",
            b"global\n  daemon\nfrontend public\n".as_slice(),
        );
        let first = lex_with_limits(&source, usize::MAX, 2);
        let second = lex_with_limits(&source, usize::MAX, 2);

        assert_eq!(first, second);
        assert_eq!(first.value().len(), 2);
        assert_eq!(first.value()[0].words[0].value, b"global");
        assert_eq!(first.value()[1].words[0].value, b"daemon");
        assert_eq!(first.diagnostics().len(), 1);
        assert_eq!(first.diagnostics()[0].code(), E_SOURCE_LIMIT);
        assert_eq!(first.diagnostics()[0].stage(), DiagnosticStage::Lex);
        assert_eq!(
            first.diagnostics()[0]
                .primary_span()
                .expect("located token limit")
                .range(),
            ByteRange::new(16, 17)
        );
    }
}
