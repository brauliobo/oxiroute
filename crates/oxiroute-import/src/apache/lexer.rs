use crate::{
    ByteRange, Diagnostic, DiagnosticStage, E_SOURCE_LIMIT, MAX_SOURCE_BYTES,
    MAX_TOKENS_PER_SOURCE, Report, Severity, SourceFile, Span,
};

use super::E_SYNTAX;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Word {
    /// Apache escape-normalized bytes.
    pub value: Vec<u8>,
    /// The exact source bytes occupied by this word.
    pub raw: Vec<u8>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Line {
    pub words: Vec<Word>,
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
    let mut diagnostics = Vec::new();
    if source.len() > max_source_bytes {
        diagnostics.push(
            Diagnostic::new(
                E_SOURCE_LIMIT,
                Severity::Error,
                DiagnosticStage::Lex,
                format!("Apache source exceeds the maximum size of {max_source_bytes} bytes"),
            )
            .with_primary_span(Span::new(
                source.id(),
                ByteRange::new(max_source_bytes.min(source.len()), source.len()),
            )),
        );
    }

    let limit = source.len().min(max_source_bytes);
    let bytes = &source.bytes()[..limit];
    let mut lines = Vec::new();
    let mut offset = 0;
    let mut token_count = 0;
    while offset < bytes.len() {
        let logical_start = offset;
        let mut logical_end;
        let mut words = Vec::new();
        loop {
            let physical_end = bytes[offset..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |index| offset + index);
            let line_end = if physical_end < bytes.len() {
                physical_end + 1
            } else {
                physical_end
            };
            let (mut line_words, continued) = lex_line(
                source,
                offset,
                physical_end,
                &mut diagnostics,
                max_tokens.saturating_sub(token_count),
            );
            token_count = token_count.saturating_add(line_words.len());
            if token_count >= max_tokens && !line_words.is_empty() {
                diagnostics.push(
                    Diagnostic::new(
                        E_SOURCE_LIMIT,
                        Severity::Error,
                        DiagnosticStage::Lex,
                        format!(
                            "Apache token count exceeds the maximum of {max_tokens} per source"
                        ),
                    )
                    .with_primary_span(line_words[0].span),
                );
                line_words.truncate(max_tokens.saturating_sub(token_count - line_words.len()));
                words.extend(line_words);
                logical_end = line_end;
                offset = bytes.len();
                break;
            }
            words.append(&mut line_words);
            logical_end = line_end;
            offset = line_end;
            if !continued || offset >= bytes.len() {
                break;
            }
        }
        if !words.is_empty() {
            lines.push(Line {
                words,
                span: Span::new(source.id(), ByteRange::new(logical_start, logical_end)),
            });
        }
    }

    Report::new(lines, diagnostics)
}

#[allow(clippy::too_many_lines)]
fn lex_line(
    source: &SourceFile,
    start: usize,
    end: usize,
    diagnostics: &mut Vec<Diagnostic>,
    remaining_tokens: usize,
) -> (Vec<Word>, bool) {
    let bytes = &source.bytes()[start..end];
    let content_end = bytes.len();
    let mut index = 0;
    let mut words = Vec::new();
    while index < content_end {
        while index < content_end && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= content_end || bytes[index] == b'#' {
            break;
        }
        if words.len() >= remaining_tokens {
            break;
        }
        let word_start = index;
        let mut value = Vec::new();
        let mut quote = None;
        while index < content_end {
            let byte = bytes[index];
            if let Some(delimiter) = quote {
                if byte == delimiter {
                    quote = None;
                    index += 1;
                    continue;
                }
                if byte == b'\\' && index + 1 < content_end {
                    value.push(bytes[index + 1]);
                    index += 2;
                    continue;
                }
                value.push(byte);
                index += 1;
                continue;
            }
            if byte == b'#' || byte.is_ascii_whitespace() {
                break;
            }
            if byte == b'\'' || byte == b'"' {
                quote = Some(byte);
                index += 1;
                continue;
            }
            if byte == b'\\' && index + 1 < content_end {
                value.push(bytes[index + 1]);
                index += 2;
                continue;
            }
            value.push(byte);
            index += 1;
        }
        if quote.is_some() {
            diagnostics.push(
                Diagnostic::new(
                    E_SYNTAX,
                    Severity::Error,
                    DiagnosticStage::Lex,
                    "unterminated Apache quoted word",
                )
                .with_primary_span(Span::new(
                    source.id(),
                    ByteRange::new(start + word_start, end),
                )),
            );
        }
        let word_end = index;
        words.push(Word {
            value,
            raw: source.bytes()[start + word_start..start + word_end].to_vec(),
            span: Span::new(
                source.id(),
                ByteRange::new(start + word_start, start + word_end),
            ),
        });
        while index < content_end && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index < content_end && bytes[index] == b'#' {
            break;
        }
    }

    let mut trailing_backslashes = 0;
    for byte in bytes
        .iter()
        .rev()
        .skip_while(|byte| byte.is_ascii_whitespace())
    {
        if *byte == b'\\' {
            trailing_backslashes += 1;
        } else {
            break;
        }
    }
    let continued = trailing_backslashes % 2 == 1;
    if continued {
        if let Some(last) = words.last_mut() {
            if last.value.last() == Some(&b'\\') {
                last.value.pop();
            }
        }
    }
    (words, continued)
}
