use crate::{
    ByteRange, Diagnostic, DiagnosticStage, E_SOURCE_LIMIT, MAX_DIRECTIVES_PER_SOURCE,
    MAX_SOURCE_BYTES, MAX_STRUCTURAL_DEPTH, MAX_TOKENS_PER_SOURCE, Report, Severity, SourceFile,
    SourceId, Span,
};

use super::{E_SYNTAX, Token, TokenKind, lexer::lex_with_limits};

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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Word {
    /// Escape-normalized bytes. The raw lexeme remains available through `span` and `SourceFile`.
    pub value: Vec<u8>,
    pub span: Span,
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
    let (tokens, mut diagnostics) =
        lex_with_limits(source, max_source_bytes, max_tokens).into_parts();
    let input_stop_offset = diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.code() == E_SOURCE_LIMIT
                && matches!(
                    diagnostic.stage(),
                    DiagnosticStage::Source | DiagnosticStage::Lex
                )
        })
        .filter_map(Diagnostic::primary_span)
        .map(|span| span.range().start())
        .min();
    let mut parser = Parser::new(
        &tokens,
        source.id(),
        source.len(),
        max_directives,
        max_structural_depth,
        input_stop_offset,
    );
    let scope = parser.parse_scope(None, 0);
    diagnostics.extend(parser.diagnostics);

    Report::new(
        Document {
            directives: scope.directives,
            span: source.full_span(),
        },
        diagnostics,
    )
}

struct Parser<'a> {
    tokens: &'a [Token],
    source: SourceId,
    source_len: usize,
    index: usize,
    max_directives: usize,
    max_structural_depth: usize,
    directive_count: usize,
    limit_reached: bool,
    stop_offset: usize,
    input_stop_offset: Option<usize>,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Parser<'a> {
    const fn new(
        tokens: &'a [Token],
        source: SourceId,
        source_len: usize,
        max_directives: usize,
        max_structural_depth: usize,
        input_stop_offset: Option<usize>,
    ) -> Self {
        Self {
            tokens,
            source,
            source_len,
            index: 0,
            max_directives,
            max_structural_depth,
            directive_count: 0,
            limit_reached: false,
            stop_offset: source_len,
            input_stop_offset,
            diagnostics: Vec::new(),
        }
    }

    fn parse_scope(&mut self, opening_brace: Option<Span>, depth: usize) -> ParsedScope {
        let mut directives = Vec::new();

        while let Some(token) = self.tokens.get(self.index) {
            if self.limit_reached {
                break;
            }
            match token.kind() {
                TokenKind::Word(_) => {
                    if self.directive_count == self.max_directives {
                        self.directive_limit(token.span());
                        break;
                    }
                    self.directive_count += 1;
                    if let Some(directive) = self.parse_directive(opening_brace.is_some(), depth) {
                        directives.push(directive);
                    } else {
                        self.directive_count -= 1;
                    }
                }
                TokenKind::CloseBrace if opening_brace.is_some() => {
                    self.index += 1;
                    return ParsedScope {
                        directives,
                        end: token.span().range().end(),
                    };
                }
                TokenKind::CloseBrace => {
                    self.syntax("unexpected closing brace", token.span());
                    self.index += 1;
                }
                TokenKind::Semicolon => {
                    self.syntax(
                        "unexpected semicolon; expected a directive name",
                        token.span(),
                    );
                    self.index += 1;
                }
                TokenKind::OpenBrace => {
                    self.syntax(
                        "unexpected opening brace; expected a directive name",
                        token.span(),
                    );
                    self.index += 1;
                }
            }
        }

        if self.limit_reached {
            return ParsedScope {
                directives,
                end: self.stop_offset,
            };
        }

        if let Some(stop_offset) = self.input_stop_offset {
            return ParsedScope {
                directives,
                end: stop_offset,
            };
        }

        if let Some(opening_brace) = opening_brace {
            self.diagnostics.push(
                Diagnostic::new(
                    E_SYNTAX,
                    Severity::Error,
                    DiagnosticStage::Parse,
                    "unexpected end of source; expected a closing brace",
                )
                .with_primary_span(self.end_span())
                .with_related_span(opening_brace, "block opened here"),
            );
        }

        ParsedScope {
            directives,
            end: self.source_len,
        }
    }

    fn parse_directive(&mut self, nested: bool, depth: usize) -> Option<Directive> {
        let name = self.take_word().expect("caller checked for a word token");
        let start = name.span.range().start();
        let mut arguments = Vec::new();

        while self
            .tokens
            .get(self.index)
            .is_some_and(|token| matches!(token.kind(), TokenKind::Word(_)))
        {
            arguments.push(self.take_word().expect("word token was checked"));
        }

        let Some(delimiter) = self.tokens.get(self.index) else {
            if self.input_stop_offset.is_some() {
                return None;
            }
            self.syntax(
                "unexpected end of source; expected `;` or `{`",
                self.end_span(),
            );
            return None;
        };

        match delimiter.kind() {
            TokenKind::Semicolon => {
                let end = delimiter.span().range().end();
                self.index += 1;
                Some(Directive {
                    name,
                    arguments,
                    children: None,
                    span: self.span(start, end),
                })
            }
            TokenKind::OpenBrace => {
                let opening_brace = delimiter.span();
                self.index += 1;
                let scope = if depth >= self.max_structural_depth {
                    self.structural_depth_limit(opening_brace);
                    ParsedScope {
                        directives: Vec::new(),
                        end: self.skip_block(opening_brace),
                    }
                } else {
                    self.parse_scope(Some(opening_brace), depth + 1)
                };
                Some(Directive {
                    name,
                    arguments,
                    children: Some(scope.directives),
                    span: self.span(start, scope.end),
                })
            }
            TokenKind::CloseBrace => {
                self.syntax("expected `;` or `{` before closing brace", delimiter.span());
                if !nested {
                    self.index += 1;
                }
                None
            }
            TokenKind::Word(_) => unreachable!("all argument words were consumed"),
        }
    }

    fn take_word(&mut self) -> Option<Word> {
        let token = self.tokens.get(self.index)?;
        let TokenKind::Word(value) = token.kind() else {
            return None;
        };
        self.index += 1;
        Some(Word {
            value: value.clone(),
            span: token.span(),
        })
    }

    fn syntax(&mut self, message: &'static str, span: Span) {
        self.diagnostics.push(
            Diagnostic::new(E_SYNTAX, Severity::Error, DiagnosticStage::Parse, message)
                .with_primary_span(span),
        );
    }

    fn directive_limit(&mut self, span: Span) {
        self.limit_reached = true;
        self.stop_offset = span.range().start();
        self.diagnostics.push(
            Diagnostic::new(
                E_SOURCE_LIMIT,
                Severity::Error,
                DiagnosticStage::Parse,
                format!(
                    "nginx directive count exceeds the maximum of {} per source",
                    self.max_directives
                ),
            )
            .with_primary_span(span),
        );
    }

    fn structural_depth_limit(&mut self, span: Span) {
        self.diagnostics.push(
            Diagnostic::new(
                E_SOURCE_LIMIT,
                Severity::Error,
                DiagnosticStage::Parse,
                format!(
                    "nginx structural block depth exceeds the maximum of {}",
                    self.max_structural_depth
                ),
            )
            .with_primary_span(span),
        );
    }

    fn skip_block(&mut self, opening_brace: Span) -> usize {
        let mut depth = 1_usize;
        while let Some(token) = self.tokens.get(self.index) {
            self.index += 1;
            match token.kind() {
                TokenKind::OpenBrace => depth = depth.saturating_add(1),
                TokenKind::CloseBrace => {
                    depth -= 1;
                    if depth == 0 {
                        return token.span().range().end();
                    }
                }
                TokenKind::Word(_) | TokenKind::Semicolon => {}
            }
        }
        if self.input_stop_offset.is_none() {
            self.diagnostics.push(
                Diagnostic::new(
                    E_SYNTAX,
                    Severity::Error,
                    DiagnosticStage::Parse,
                    "unexpected end of source; expected a closing brace",
                )
                .with_primary_span(self.end_span())
                .with_related_span(opening_brace, "block opened here"),
            );
        }
        self.input_stop_offset.unwrap_or(self.source_len)
    }

    const fn end_span(&self) -> Span {
        self.span(self.source_len, self.source_len)
    }

    const fn span(&self, start: usize, end: usize) -> Span {
        Span::new(self.source, ByteRange::new(start, end))
    }
}

struct ParsedScope {
    directives: Vec<Directive>,
    end: usize,
}

#[cfg(test)]
mod tests {
    use crate::{ByteRange, DiagnosticStage, E_SOURCE_LIMIT, SourceFile, SourceId};

    use super::{Directive, parse_with_limits};

    #[test]
    fn directive_limit_keeps_a_deterministic_bounded_ast() {
        let boundary = source("outer { a; b; }");
        let boundary_report = parse_with_limits(&boundary, usize::MAX, usize::MAX, 3, usize::MAX);
        assert_eq!(directive_count(&boundary_report.value().directives), 3);
        assert!(boundary_report.diagnostics().is_empty());

        let over = source("outer { a; b; c; }");
        let first = parse_with_limits(&over, usize::MAX, usize::MAX, 3, usize::MAX);
        let second = parse_with_limits(&over, usize::MAX, usize::MAX, 3, usize::MAX);

        assert_eq!(first, second);
        assert_eq!(directive_count(&first.value().directives), 3);
        assert_eq!(first.diagnostics().len(), 1);
        assert_eq!(first.diagnostics()[0].code(), E_SOURCE_LIMIT);
        assert_eq!(first.diagnostics()[0].stage(), DiagnosticStage::Parse);
        assert_eq!(
            first.diagnostics()[0]
                .primary_span()
                .expect("located directive limit")
                .range(),
            ByteRange::new(14, 15)
        );
        assert_eq!(
            first.value().directives[0].span.range(),
            ByteRange::new(0, 14)
        );
    }

    #[test]
    fn lexical_limit_does_not_create_a_parser_error() {
        let source = source("a; b;");
        let report = parse_with_limits(&source, usize::MAX, 3, usize::MAX, usize::MAX);

        assert_eq!(report.value().directives.len(), 1);
        assert_eq!(report.value().directives[0].name.value, b"a");
        assert_eq!(report.diagnostics().len(), 1);
        assert_eq!(report.diagnostics()[0].code(), E_SOURCE_LIMIT);
        assert_eq!(report.diagnostics()[0].stage(), DiagnosticStage::Lex);
        assert_eq!(
            report.diagnostics()[0]
                .primary_span()
                .expect("located token limit")
                .range(),
            ByteRange::new(4, 5)
        );
    }

    fn source(contents: &str) -> SourceFile {
        SourceFile::new(SourceId::new(1), "nginx.conf", contents.as_bytes())
    }

    fn directive_count(directives: &[Directive]) -> usize {
        directives
            .iter()
            .map(|directive| 1 + directive.children.as_deref().map_or(0, directive_count))
            .sum()
    }
}
