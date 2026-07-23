use oxiroute_import::{
    ByteRange, Diagnostic, DiagnosticCode, DiagnosticStage, Report, Severity, SourceFile, SourceId,
    Span,
};

const E_ALPHA: DiagnosticCode = DiagnosticCode::new("E_ALPHA");
const E_ZULU: DiagnosticCode = DiagnosticCode::new("E_ZULU");

#[test]
fn source_files_are_immutable_byte_sources_with_checked_spans() {
    let source = SourceFile::new(SourceId::new(7), "nginx.conf", b"listen 80;".as_slice());
    let range = ByteRange::new(7, 9);
    let span = Span::new(SourceId::new(7), range);

    assert_eq!(source.id(), SourceId::new(7));
    assert_eq!(source.name(), "nginx.conf");
    assert_eq!(source.path(), None);
    assert_eq!(source.bytes(), b"listen 80;");
    assert_eq!(source.len(), 10);
    assert!(!source.is_empty());
    assert_eq!(source.slice(range), Some(b"80".as_slice()));
    assert_eq!(source.span(range), Some(span));
    assert_eq!(source.span(ByteRange::new(9, 11)), None);
    assert_eq!(
        source.full_span(),
        Span::new(SourceId::new(7), ByteRange::new(0, 10))
    );

    let cloned = source.clone();
    assert_eq!(cloned.bytes(), source.bytes());
}

#[test]
fn byte_ranges_are_half_open_and_measure_bytes() {
    let range = ByteRange::new(3, 8);

    assert_eq!(range.start(), 3);
    assert_eq!(range.end(), 8);
    assert_eq!(range.len(), 5);
    assert!(!range.is_empty());
    assert!(range.contains(3));
    assert!(range.contains(7));
    assert!(!range.contains(8));
}

#[test]
#[should_panic(expected = "byte range start must not exceed its end")]
fn byte_ranges_reject_reversed_bounds() {
    let _ = ByteRange::new(2, 1);
}

#[test]
fn reports_sort_diagnostics_by_location_then_stable_fields() {
    let source = SourceId::new(2);
    let early = Span::new(source, ByteRange::new(2, 3));
    let late = Span::new(source, ByteRange::new(8, 9));
    let report = Report::new(
        "partial output",
        vec![
            Diagnostic::new(E_ZULU, Severity::Error, DiagnosticStage::Parse, "late")
                .with_primary_span(late),
            Diagnostic::new(
                E_ZULU,
                Severity::Warning,
                DiagnosticStage::Lower,
                "early zulu",
            )
            .with_primary_span(early),
            Diagnostic::new(
                E_ALPHA,
                Severity::Error,
                DiagnosticStage::Parse,
                "early alpha",
            )
            .with_primary_span(early)
            .with_help("correct the source")
            .with_related_span(late, "related declaration"),
        ],
    );

    assert_eq!(report.value(), &"partial output");
    assert!(report.has_errors());
    assert_eq!(
        report
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.code().as_str())
            .collect::<Vec<_>>(),
        ["E_ALPHA", "E_ZULU", "E_ZULU"]
    );
    assert_eq!(report.diagnostics()[0].help(), Some("correct the source"));
    assert_eq!(report.diagnostics()[0].related_spans().len(), 1);
    assert_eq!(report.diagnostics()[2].primary_span(), Some(late));
}

#[test]
fn diagnostic_order_does_not_depend_on_producer_insertion_order() {
    let span = Span::new(SourceId::new(1), ByteRange::new(0, 1));
    let diagnostic = |help| {
        Diagnostic::new(
            E_ALPHA,
            Severity::Error,
            DiagnosticStage::Parse,
            "same diagnostic",
        )
        .with_primary_span(span)
        .with_help(help)
    };
    let report = Report::new((), vec![diagnostic("z help"), diagnostic("a help")]);

    assert_eq!(report.diagnostics()[0].help(), Some("a help"));
    assert_eq!(report.diagnostics()[1].help(), Some("z help"));
}

#[test]
fn same_location_diagnostics_sort_by_severity_before_code() {
    let span = Span::new(SourceId::new(1), ByteRange::new(0, 1));
    let report = Report::new(
        (),
        vec![
            Diagnostic::new(
                E_ALPHA,
                Severity::Warning,
                DiagnosticStage::Parse,
                "warning",
            )
            .with_primary_span(span),
            Diagnostic::new(E_ZULU, Severity::Error, DiagnosticStage::Parse, "error")
                .with_primary_span(span),
        ],
    );

    assert_eq!(report.diagnostics()[0].severity(), Severity::Error);
    assert_eq!(report.diagnostics()[0].code(), E_ZULU);
    assert_eq!(report.diagnostics()[1].severity(), Severity::Warning);
    assert_eq!(report.diagnostics()[1].code(), E_ALPHA);
}
