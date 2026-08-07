use crate::{
    ByteRange, Diagnostic, DiagnosticStage, E_SOURCE_LIMIT, MAX_SOURCE_BYTES,
    MAX_TOKENS_PER_SOURCE, Report, Severity, SourceFile, SourceId, Span,
};

use super::{E_SYNTAX, MAX_WORDS_PER_LINE};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuoteStyle {
    None,
    Single,
    Double,
    Mixed,
}

/// One Squid word after quote removal and backslash decoding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Word {
    pub value: Vec<u8>,
    pub span: Span,
    pub quote_style: QuoteStyle,
}

/// One logical Squid line. Its span includes continued physical lines and their endings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Line {
    pub words: Vec<Word>,
    pub comments: Vec<Span>,
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
    let mut diagnostics = Vec::new();
    if bounded_len < source.len() {
        diagnostics.push(
            Diagnostic::new(
                E_SOURCE_LIMIT,
                Severity::Error,
                DiagnosticStage::Source,
                format!("Squid source exceeds the maximum size of {max_source_bytes} bytes"),
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
        max_tokens,
        token_count: 0,
        cursor: 0,
        lines: Vec::new(),
        diagnostics,
    }
    .run()
}

struct Lexer<'a> {
    bytes: &'a [u8],
    source: SourceId,
    max_tokens: usize,
    token_count: usize,
    cursor: usize,
    lines: Vec<Line>,
    diagnostics: Vec<Diagnostic>,
}

impl Lexer<'_> {
    fn run(mut self) -> Report<Vec<Line>> {
        while self.cursor < self.bytes.len() {
            let start = self.cursor;
            match self.lex_logical_line(start) {
                Ok((line, next)) => {
                    self.lines.push(line);
                    self.cursor = next;
                }
                Err(next) => self.cursor = next.max(self.cursor + 1),
            }
        }
        Report::new(self.lines, self.diagnostics)
    }

    fn lex_logical_line(&mut self, start: usize) -> Result<(Line, usize), usize> {
        let mut words = Vec::new();
        let mut comments = Vec::new();
        let mut cursor = start;
        let mut logical_end;

        loop {
            let physical_end = self.physical_end(cursor);
            let content_end = self.content_end(cursor, physical_end);
            let next = physical_end + usize::from(physical_end < self.bytes.len());
            if let Some(nul) = self.bytes[cursor..content_end]
                .iter()
                .position(|byte| *byte == b'\0')
            {
                let offset = cursor + nul;
                self.syntax(
                    "NUL byte is not permitted in Squid source",
                    offset,
                    offset + 1,
                );
                return Err(next);
            }

            let continuation = self.continuation_offset(cursor, content_end);
            let segment_end = continuation.unwrap_or(content_end);
            if self.lex_segment(cursor, segment_end, &mut words, &mut comments)? {
                logical_end = next;
                return Ok((
                    Line {
                        words,
                        comments,
                        span: self.span(start, logical_end),
                    },
                    next,
                ));
            }
            logical_end = next;
            if continuation.is_none() || next == self.bytes.len() {
                break;
            }
            cursor = next;
        }

        Ok((
            Line {
                words,
                comments,
                span: self.span(start, logical_end),
            },
            logical_end,
        ))
    }

    /// Returns true when an unquoted comment terminated the logical line.
    fn lex_segment(
        &mut self,
        start: usize,
        end: usize,
        words: &mut Vec<Word>,
        comments: &mut Vec<Span>,
    ) -> Result<bool, usize> {
        let mut cursor = start;
        while cursor < end {
            while self
                .bytes
                .get(cursor)
                .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\r'))
            {
                cursor += 1;
            }
            if cursor == end {
                return Ok(false);
            }
            if self.bytes[cursor] == b'#' {
                comments.push(self.span(cursor, end));
                return Ok(true);
            }
            if words.len() == MAX_WORDS_PER_LINE {
                self.limit(
                    format!("Squid logical line exceeds the maximum of {MAX_WORDS_PER_LINE} words"),
                    cursor,
                );
                return Err(end);
            }
            if self.token_count == self.max_tokens {
                self.limit(
                    format!(
                        "Squid token count exceeds the maximum of {} per source",
                        self.max_tokens
                    ),
                    cursor,
                );
                return Err(end);
            }
            let (word, next) = self.lex_word(cursor, end)?;
            self.token_count += 1;
            words.push(word);
            cursor = next;
        }
        Ok(false)
    }

    fn lex_word(&mut self, start: usize, end: usize) -> Result<(Word, usize), usize> {
        let mut cursor = start;
        let mut value = Vec::new();
        let mut quote: Option<(u8, usize)> = None;
        let mut style = QuoteStyle::None;

        while cursor < end {
            let byte = self.bytes[cursor];
            match quote {
                Some((delimiter, _)) if byte == delimiter => {
                    quote = None;
                    cursor += 1;
                }
                None if matches!(byte, b' ' | b'\t' | b'\r') => break,
                None if byte == b'#' => break,
                None if matches!(byte, b'\'' | b'"') => {
                    let next_style = if byte == b'\'' {
                        QuoteStyle::Single
                    } else {
                        QuoteStyle::Double
                    };
                    style = match style {
                        QuoteStyle::None => next_style,
                        current if current == next_style => current,
                        _ => QuoteStyle::Mixed,
                    };
                    quote = Some((byte, cursor));
                    cursor += 1;
                }
                Some(_) | None if byte == b'\\' => {
                    self.decode_escape(&mut cursor, end, &mut value)?;
                }
                _ => {
                    value.push(byte);
                    cursor += 1;
                }
            }
        }

        if let Some((_, quote_start)) = quote {
            self.syntax(
                "unterminated quote in Squid word",
                quote_start,
                end.max(quote_start + 1),
            );
            return Err(end);
        }
        if value.is_empty() {
            self.syntax(
                "empty Squid word is not permitted",
                start,
                cursor.max(start + 1),
            );
            return Err(end);
        }
        Ok((
            Word {
                value,
                span: self.span(start, cursor),
                quote_style: style,
            },
            cursor,
        ))
    }

    fn decode_escape(
        &mut self,
        cursor: &mut usize,
        end: usize,
        value: &mut Vec<u8>,
    ) -> Result<(), usize> {
        let start = *cursor;
        let Some(&escaped) = self.bytes.get(start + 1).filter(|_| start + 1 < end) else {
            self.syntax("trailing escape in Squid word", start, start + 1);
            return Err(end);
        };
        value.push(escaped);
        *cursor += 2;
        Ok(())
    }

    fn physical_end(&self, start: usize) -> usize {
        self.bytes[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(self.bytes.len(), |offset| start + offset)
    }

    fn content_end(&self, start: usize, physical_end: usize) -> usize {
        if physical_end > start && self.bytes[physical_end - 1] == b'\r' {
            physical_end - 1
        } else {
            physical_end
        }
    }

    fn continuation_offset(&self, start: usize, end: usize) -> Option<usize> {
        let mut cursor = end;
        while cursor > start && matches!(self.bytes[cursor - 1], b' ' | b'\t') {
            cursor -= 1;
        }
        (cursor > start && self.bytes[cursor - 1] == b'\\').then_some(cursor - 1)
    }

    fn syntax(&mut self, message: impl Into<String>, start: usize, end: usize) {
        self.diagnostics.push(
            Diagnostic::new(E_SYNTAX, Severity::Error, DiagnosticStage::Lex, message)
                .with_primary_span(self.span(start, end)),
        );
    }

    fn limit(&mut self, message: impl Into<String>, offset: usize) {
        self.diagnostics.push(
            Diagnostic::new(
                E_SOURCE_LIMIT,
                Severity::Error,
                DiagnosticStage::Lex,
                message,
            )
            .with_primary_span(self.span(offset, offset + 1)),
        );
    }

    fn span(&self, start: usize, end: usize) -> Span {
        Span::new(self.source, ByteRange::new(start, end))
    }
}
