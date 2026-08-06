use crate::{
    ByteRange, Diagnostic, DiagnosticStage, E_SOURCE_LIMIT, MAX_DIRECTIVES_PER_SOURCE,
    MAX_SOURCE_BYTES, MAX_TOKENS_PER_SOURCE, Report, Severity, SourceFile, Span,
};

use super::{Line, Word, lexer::lex_with_limits};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Directive {
    pub name: Word,
    pub arguments: Vec<Word>,
    pub comments: Vec<Span>,
    pub span: Span,
    pub line_span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Document {
    pub lines: Vec<Line>,
    pub directives: Vec<Directive>,
    pub span: Span,
}

#[must_use]
pub fn parse(source: &SourceFile) -> Report<Document> {
    parse_with_limits(
        source,
        MAX_SOURCE_BYTES,
        MAX_TOKENS_PER_SOURCE,
        MAX_DIRECTIVES_PER_SOURCE,
    )
}

pub(super) fn parse_with_limits(
    source: &SourceFile,
    max_source_bytes: usize,
    max_tokens: usize,
    max_directives: usize,
) -> Report<Document> {
    let (lines, mut diagnostics) =
        lex_with_limits(source, max_source_bytes, max_tokens).into_parts();
    let mut directives = Vec::with_capacity(lines.len().min(max_directives));
    for line in &lines {
        let Some(name) = line.words.first() else {
            continue;
        };
        if directives.len() == max_directives {
            diagnostics.push(
                Diagnostic::new(
                    E_SOURCE_LIMIT,
                    Severity::Error,
                    DiagnosticStage::Parse,
                    format!(
                        "Squid directive count exceeds the maximum of {max_directives} per source"
                    ),
                )
                .with_primary_span(name.span),
            );
            break;
        }
        let end = line
            .words
            .last()
            .map_or(name.span.range().end(), |word| word.span.range().end());
        directives.push(Directive {
            name: name.clone(),
            arguments: line.words[1..].to_vec(),
            comments: line.comments.clone(),
            span: Span::new(source.id(), ByteRange::new(name.span.range().start(), end)),
            line_span: line.span,
        });
    }
    Report::new(
        Document {
            lines,
            directives,
            span: source.full_span(),
        },
        diagnostics,
    )
}
