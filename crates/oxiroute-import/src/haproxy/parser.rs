use std::path::{Path, PathBuf};

use crate::{
    ByteRange, Diagnostic, DiagnosticStage, E_SOURCE_LIMIT, MAX_DIRECTIVES_PER_SOURCE,
    MAX_SOURCE_BYTES, MAX_TOKENS_PER_SOURCE, Report, Severity, SourceFile, Span,
};

use super::{E_CONDITIONAL_PREPROCESSING, E_ENVIRONMENT_EXPANSION};
use super::{
    Line, Word,
    lexer::lex_with_limits,
    source_roots::{LoadedSource, RootLoadDecision},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Document {
    /// All successfully lexed physical lines, including blank and comment-only lines.
    pub lines: Vec<Line>,
    /// Statements before the first recognized section header.
    pub preamble: Vec<Directive>,
    pub sections: Vec<Section>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Directive {
    pub name: Word,
    pub arguments: Vec<Word>,
    pub comment: Option<Span>,
    /// The first word through the last word, excluding whitespace and comments.
    pub span: Span,
    /// The complete physical source line, including its original line ending.
    pub line_span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Section {
    pub kind: SectionKind,
    pub header: Directive,
    pub directives: Vec<Directive>,
    /// The section header line through the byte before the next section or file boundary.
    pub span: Span,
}

/// Every current section starter registered by `HAProxy`, plus retained legacy starters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SectionKind {
    Global,
    Defaults,
    Frontend,
    Backend,
    Listen,
    Userlist,
    Peers,
    Mailers,
    NamespaceList,
    Traces,
    Resolvers,
    Cache,
    FcgiApp,
    Ring,
    LogForward,
    LogProfile,
    HttpErrors,
    CrtStore,
    Acme,
    Healthcheck,
    Program,
}

impl SectionKind {
    fn from_keyword(keyword: &[u8]) -> Option<Self> {
        match keyword {
            b"global" => Some(Self::Global),
            b"defaults" => Some(Self::Defaults),
            b"frontend" => Some(Self::Frontend),
            b"backend" => Some(Self::Backend),
            b"listen" => Some(Self::Listen),
            b"userlist" => Some(Self::Userlist),
            b"peers" => Some(Self::Peers),
            b"mailers" => Some(Self::Mailers),
            b"namespace_list" => Some(Self::NamespaceList),
            b"traces" => Some(Self::Traces),
            b"resolvers" => Some(Self::Resolvers),
            b"cache" => Some(Self::Cache),
            b"fcgi-app" => Some(Self::FcgiApp),
            b"ring" => Some(Self::Ring),
            b"log-forward" => Some(Self::LogForward),
            b"log-profile" => Some(Self::LogProfile),
            b"http-errors" => Some(Self::HttpErrors),
            b"crt-store" => Some(Self::CrtStore),
            b"acme" => Some(Self::Acme),
            b"healthcheck" => Some(Self::Healthcheck),
            b"program" => Some(Self::Program),
            _ => None,
        }
    }
}

/// One parsed occurrence in an ordered multi-file `HAProxy` configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedSource {
    pub root_ordinal: usize,
    pub file_ordinal: usize,
    pub path: PathBuf,
    pub source: SourceFile,
    pub document: Document,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Configuration {
    pub files: Vec<ParsedSource>,
    pub root_decisions: Vec<RootLoadDecision>,
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

/// Parses already loaded occurrences independently so a section can never cross a file boundary.
#[must_use]
pub fn parse_sources(sources: &[LoadedSource]) -> Report<Configuration> {
    let mut files = Vec::with_capacity(sources.len());
    let mut diagnostics = Vec::new();

    for loaded in sources {
        let (document, source_diagnostics) = parse(&loaded.source).into_parts();
        diagnostics.extend(source_diagnostics);
        files.push(ParsedSource {
            root_ordinal: loaded.root_ordinal,
            file_ordinal: loaded.file_ordinal,
            path: loaded.path.clone(),
            source: loaded.source.clone(),
            document,
        });
    }

    Report::new(
        Configuration {
            files,
            root_decisions: Vec::new(),
        },
        diagnostics,
    )
}

/// Loads and structurally parses repeated `HAProxy` `-f` roots.
#[must_use]
pub fn parse_roots<P: AsRef<Path>>(roots: &[P]) -> Report<Configuration> {
    let (sources, mut diagnostics) = super::load_roots(roots).into_parts();
    let mut configuration = Configuration {
        files: Vec::new(),
        root_decisions: sources.decisions.clone(),
    };
    if sources.complete() {
        let (parsed, parse_diagnostics) = parse_sources(&sources).into_parts();
        configuration.files = parsed.files;
        diagnostics.extend(parse_diagnostics);
    }
    Report::new(configuration, diagnostics)
}

fn parse_with_limits(
    source: &SourceFile,
    max_source_bytes: usize,
    max_tokens: usize,
    max_directives: usize,
) -> Report<Document> {
    let (lines, mut diagnostics) =
        lex_with_limits(source, max_source_bytes, max_tokens).into_parts();
    diagnostics.extend(preprocessing_diagnostics(&lines));
    let stop_offset = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code() == E_SOURCE_LIMIT)
        .filter_map(Diagnostic::primary_span)
        .map(|span| span.range().start())
        .min()
        .unwrap_or(source.len());
    let (preamble, sections, parser_diagnostics) = parse_lines(&lines, max_directives, stop_offset);
    diagnostics.extend(parser_diagnostics);

    Report::new(
        Document {
            lines,
            preamble,
            sections,
            span: source.full_span(),
        },
        diagnostics,
    )
}

fn preprocessing_diagnostics(lines: &[Line]) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    for line in lines {
        for reference in line
            .words
            .iter()
            .flat_map(|word| &word.environment_references)
        {
            diagnostics.push(
                Diagnostic::new(
                    E_ENVIRONMENT_EXPANSION,
                    Severity::Error,
                    DiagnosticStage::Resolve,
                    "HAProxy environment reference requires explicit preprocessing before activation",
                )
                .with_primary_span(*reference),
            );
        }

        let Some(keyword) = line.words.first() else {
            continue;
        };
        if matches!(
            keyword.value.as_slice(),
            b".if" | b".elif" | b".else" | b".endif"
        ) {
            diagnostics.push(
                Diagnostic::new(
                    E_CONDITIONAL_PREPROCESSING,
                    Severity::Error,
                    DiagnosticStage::Resolve,
                    "HAProxy conditional requires explicit preprocessing before activation",
                )
                .with_primary_span(keyword.span),
            );
        }
    }

    diagnostics
}

fn parse_lines(
    lines: &[Line],
    max_directives: usize,
    mut stop_offset: usize,
) -> (Vec<Directive>, Vec<Section>, Vec<Diagnostic>) {
    let mut preamble = Vec::new();
    let mut sections = Vec::new();
    let mut current_section: Option<SectionBuilder> = None;
    let mut diagnostics = Vec::new();
    let mut directive_count = 0;

    for line in lines {
        let Some(first_word) = line.words.first() else {
            continue;
        };
        if directive_count == max_directives {
            stop_offset = line.span.range().start();
            diagnostics.push(
                Diagnostic::new(
                    E_SOURCE_LIMIT,
                    Severity::Error,
                    DiagnosticStage::Parse,
                    format!(
                        "HAProxy directive count exceeds the maximum of {max_directives} per source"
                    ),
                )
                .with_primary_span(first_word.span),
            );
            break;
        }
        directive_count += 1;

        let directive = directive_from_line(line);
        if let Some(kind) = SectionKind::from_keyword(&directive.name.value) {
            if let Some(section) = current_section.take() {
                sections.push(section.finish(line.span.range().start()));
            }
            current_section = Some(SectionBuilder {
                kind,
                start: line.span.range().start(),
                header: directive,
                directives: Vec::new(),
            });
        } else if let Some(section) = &mut current_section {
            section.directives.push(directive);
        } else {
            preamble.push(directive);
        }
    }

    if let Some(section) = current_section {
        sections.push(section.finish(stop_offset));
    }

    (preamble, sections, diagnostics)
}

fn directive_from_line(line: &Line) -> Directive {
    let mut words = line.words.iter().cloned();
    let name = words.next().expect("caller checked for a non-empty line");
    let arguments: Vec<_> = words.collect();
    let end = arguments
        .last()
        .map_or(name.span.range().end(), |word| word.span.range().end());

    Directive {
        span: Span::new(
            name.span.source(),
            ByteRange::new(name.span.range().start(), end),
        ),
        name,
        arguments,
        comment: line.comment,
        line_span: line.span,
    }
}

struct SectionBuilder {
    kind: SectionKind,
    start: usize,
    header: Directive,
    directives: Vec<Directive>,
}

impl SectionBuilder {
    fn finish(self, end: usize) -> Section {
        Section {
            kind: self.kind,
            span: Span::new(self.header.span.source(), ByteRange::new(self.start, end)),
            header: self.header,
            directives: self.directives,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{ByteRange, DiagnosticStage, E_SOURCE_LIMIT, SourceFile, SourceId};

    use super::parse_with_limits;

    #[test]
    fn directive_limit_closes_the_partial_section_at_the_bounded_prefix() {
        let source = SourceFile::new(
            SourceId::new(1),
            "haproxy.cfg",
            b"frontend public\n  bind :80\n  mode http\n".as_slice(),
        );
        let first = parse_with_limits(&source, usize::MAX, usize::MAX, 2);
        let second = parse_with_limits(&source, usize::MAX, usize::MAX, 2);

        assert_eq!(first, second);
        assert_eq!(first.value().sections.len(), 1);
        assert_eq!(first.value().sections[0].directives.len(), 1);
        assert_eq!(
            first.value().sections[0].span.range(),
            ByteRange::new(0, 27)
        );
        assert_eq!(first.diagnostics().len(), 1);
        assert_eq!(first.diagnostics()[0].code(), E_SOURCE_LIMIT);
        assert_eq!(first.diagnostics()[0].stage(), DiagnosticStage::Parse);
        assert_eq!(
            first.diagnostics()[0]
                .primary_span()
                .expect("located directive limit")
                .range(),
            ByteRange::new(29, 33)
        );
    }
}
