use crate::{
    ByteRange, Diagnostic, DiagnosticStage, MAX_DIRECTIVES_PER_SOURCE, MAX_SOURCE_BYTES,
    MAX_STRUCTURAL_DEPTH, MAX_TOKENS_PER_SOURCE, Report, Severity, SourceFile, Span,
};

use super::{E_SYNTAX, Line, Word, lexer::lex_with_limits};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Document {
    pub directives: Vec<Directive>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Directive {
    pub name: Word,
    pub arguments: Vec<Word>,
    pub children: Option<Vec<Self>>,
    pub span: Span,
    pub line_span: Span,
}

#[must_use]
pub fn parse(source: &SourceFile) -> Report<Document> {
    parse_with_limits(
        source,
        MAX_SOURCE_BYTES,
        MAX_TOKENS_PER_SOURCE,
        MAX_DIRECTIVES_PER_SOURCE,
        MAX_STRUCTURAL_DEPTH,
    )
}

pub(super) fn parse_with_limits(
    source: &SourceFile,
    max_source_bytes: usize,
    max_tokens: usize,
    max_directives: usize,
    max_structural_depth: usize,
) -> Report<Document> {
    let (lines, mut diagnostics) =
        lex_with_limits(source, max_source_bytes, max_tokens).into_parts();
    let mut parser = Parser {
        source,
        max_directives,
        max_structural_depth,
        directive_count: 0,
        diagnostics: Vec::new(),
        roots: Vec::new(),
        stack: Vec::new(),
    };
    for line in lines {
        parser.parse_line(&line);
    }
    parser.finish();
    diagnostics.extend(parser.diagnostics);
    Report::new(
        Document {
            directives: parser.roots,
            span: source.full_span(),
        },
        diagnostics,
    )
}

struct Parser<'a> {
    source: &'a SourceFile,
    max_directives: usize,
    max_structural_depth: usize,
    directive_count: usize,
    diagnostics: Vec<Diagnostic>,
    roots: Vec<Directive>,
    stack: Vec<BlockBuilder>,
}

struct BlockBuilder {
    name: Word,
    arguments: Vec<Word>,
    children: Vec<Directive>,
    start: usize,
}

impl Parser<'_> {
    fn parse_line(&mut self, line: &Line) {
        if line.words.is_empty() {
            return;
        }
        if self.directive_count >= self.max_directives {
            self.diagnostics.push(
                Diagnostic::new(
                    crate::E_SOURCE_LIMIT,
                    Severity::Error,
                    DiagnosticStage::Parse,
                    format!(
                        "Apache directive count exceeds the maximum of {} per source",
                        self.max_directives
                    ),
                )
                .with_primary_span(line.span),
            );
            return;
        }
        self.directive_count += 1;

        if line.words[0].value.starts_with(b"</") {
            self.close_block(line);
        } else if line.words[0].value.starts_with(b"<") {
            self.open_block(line);
        } else {
            let directive = Directive {
                name: line.words[0].clone(),
                arguments: line.words[1..].to_vec(),
                children: None,
                span: line.span,
                line_span: line.span,
            };
            self.push_directive(directive);
        }
    }

    fn open_block(&mut self, line: &Line) {
        let Some((name, arguments)) = parse_open_tag(&line.words) else {
            self.syntax("malformed Apache block opening", line.span);
            return;
        };
        if self.stack.len() >= self.max_structural_depth {
            self.diagnostics.push(
                Diagnostic::new(
                    crate::E_SOURCE_LIMIT,
                    Severity::Error,
                    DiagnosticStage::Parse,
                    format!(
                        "Apache structural block depth exceeds the maximum of {}",
                        self.max_structural_depth
                    ),
                )
                .with_primary_span(line.span),
            );
        }
        self.stack.push(BlockBuilder {
            name,
            arguments,
            children: Vec::new(),
            start: line.span.range().start(),
        });
    }

    fn close_block(&mut self, line: &Line) {
        let Some(closing_name) = parse_close_tag(&line.words) else {
            self.syntax("malformed Apache block closing", line.span);
            return;
        };
        let Some(builder) = self.stack.pop() else {
            self.syntax("Apache block closes without an opening tag", line.span);
            return;
        };
        if !builder.name.value.eq_ignore_ascii_case(&closing_name.value) {
            self.diagnostics.push(
                Diagnostic::new(
                    E_SYNTAX,
                    Severity::Error,
                    DiagnosticStage::Parse,
                    "Apache block closing tag does not match its opening tag",
                )
                .with_primary_span(line.span)
                .with_related_span(builder.name.span, "block opened here"),
            );
        }
        let directive = Directive {
            name: builder.name,
            arguments: builder.arguments,
            children: Some(builder.children),
            span: Span::new(
                self.source.id(),
                ByteRange::new(builder.start, line.span.range().end()),
            ),
            line_span: Span::new(
                self.source.id(),
                ByteRange::new(builder.start, line.span.range().end()),
            ),
        };
        self.push_directive(directive);
    }

    fn push_directive(&mut self, directive: Directive) {
        if let Some(parent) = self.stack.last_mut() {
            parent.children.push(directive);
        } else {
            self.roots.push(directive);
        }
    }

    fn finish(&mut self) {
        while let Some(builder) = self.stack.pop() {
            self.diagnostics.push(
                Diagnostic::new(
                    E_SYNTAX,
                    Severity::Error,
                    DiagnosticStage::Parse,
                    "Apache block is missing its closing tag",
                )
                .with_primary_span(self.source.full_span())
                .with_related_span(builder.name.span, "block opened here"),
            );
        }
    }

    fn syntax(&mut self, message: &'static str, span: Span) {
        self.diagnostics.push(
            Diagnostic::new(E_SYNTAX, Severity::Error, DiagnosticStage::Parse, message)
                .with_primary_span(span),
        );
    }
}

fn parse_open_tag(words: &[Word]) -> Option<(Word, Vec<Word>)> {
    let first = words.first()?;
    let mut name = first.value.strip_prefix(b"<")?.to_vec();
    if name.ends_with(b">") {
        name.pop();
    }
    if name.is_empty() || name.starts_with(b"/") {
        return None;
    }
    let mut arguments = words[1..].to_vec();
    if let Some(last) = arguments.last_mut() {
        if last.value == b">" {
            arguments.pop();
        } else if last.value.ends_with(b">") {
            last.value.pop();
            last.raw.pop();
        }
    }
    Some((
        Word {
            value: name,
            raw: first.raw.clone(),
            span: first.span,
        },
        arguments,
    ))
}

fn parse_close_tag(words: &[Word]) -> Option<Word> {
    let word = words.first()?.clone();
    if words.len() != 1 {
        return None;
    }
    let value = word.value.strip_prefix(b"</")?.strip_suffix(b">")?;
    (!value.is_empty()).then(|| Word {
        value: value.to_vec(),
        raw: word.raw,
        span: word.span,
    })
}
