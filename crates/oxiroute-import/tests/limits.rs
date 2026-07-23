use oxiroute_import::{
    ByteRange, DiagnosticStage, E_SOURCE_LIMIT, MAX_AGGREGATE_SOURCE_BYTES,
    MAX_DIRECTIVES_PER_SOURCE, MAX_EXPANDED_DIRECTIVES, MAX_GLOB_MATCHES, MAX_SOURCE_BYTES,
    MAX_SOURCE_FILES, MAX_STRUCTURAL_DEPTH, MAX_TOKENS_PER_SOURCE, SourceFile, SourceId,
    nginx::{lex, parse},
};

#[test]
fn public_single_source_limits_are_stable() {
    assert_eq!(MAX_SOURCE_BYTES, 1024 * 1024);
    assert_eq!(MAX_TOKENS_PER_SOURCE, 1_000_000);
    assert_eq!(MAX_DIRECTIVES_PER_SOURCE, 250_000);
    assert_eq!(MAX_STRUCTURAL_DEPTH, 256);
    assert_eq!(MAX_GLOB_MATCHES, 100_000);
    assert_eq!(MAX_EXPANDED_DIRECTIVES, 1_000_000);
    assert_eq!(MAX_SOURCE_FILES, 4_096);
    assert_eq!(MAX_AGGREGATE_SOURCE_BYTES, 64 * 1024 * 1024);
}

#[test]
fn source_byte_limit_accepts_the_boundary() {
    let mut bytes = vec![b' '; MAX_SOURCE_BYTES];
    bytes[0..2].copy_from_slice(b"a;");
    let source = SourceFile::new(SourceId::new(1), "boundary.conf", bytes);
    let report = parse(&source);

    assert!(report.diagnostics().is_empty());
    assert_eq!(report.value().directives.len(), 1);
    assert_eq!(report.value().directives[0].name.value, b"a");
}

#[test]
fn source_byte_limit_reports_excess_and_keeps_the_completed_prefix() {
    let mut bytes = vec![b' '; MAX_SOURCE_BYTES + 1];
    bytes[0..2].copy_from_slice(b"a;");
    bytes[MAX_SOURCE_BYTES - 1] = b'b';
    bytes[MAX_SOURCE_BYTES] = b';';
    let source = SourceFile::new(SourceId::new(1), "over.conf", bytes);

    let lexed = lex(&source);
    assert_eq!(lexed.value().len(), 2);
    assert_eq!(lexed.diagnostics().len(), 1);
    assert_eq!(lexed.diagnostics()[0].code(), E_SOURCE_LIMIT);
    assert_eq!(lexed.diagnostics()[0].stage(), DiagnosticStage::Source);
    assert_eq!(
        lexed.diagnostics()[0]
            .primary_span()
            .expect("located source limit")
            .range(),
        ByteRange::new(MAX_SOURCE_BYTES, MAX_SOURCE_BYTES + 1)
    );

    let first = parse(&source);
    let second = parse(&source);
    assert_eq!(first, second);
    assert_eq!(first.value().directives.len(), 1);
    assert_eq!(first.value().directives[0].name.value, b"a");
    assert_eq!(first.diagnostics(), lexed.diagnostics());
}
