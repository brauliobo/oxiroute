use oxiroute_import::{
    ByteRange, DiagnosticStage, SourceFile, SourceId,
    nginx::{E_SYNTAX, TokenKind, lex},
};

#[test]
fn token_spans_are_half_open_source_byte_ranges() {
    let source = source("listen 8080;\n");
    let report = lex(&source);
    let tokens = report.value();

    assert!(report.diagnostics().is_empty());
    assert_eq!(tokens.len(), 3);
    assert_eq!(tokens[0].kind(), &TokenKind::Word(b"listen".to_vec()));
    assert_eq!(tokens[0].span().range(), ByteRange::new(0, 6));
    assert_eq!(tokens[1].kind(), &TokenKind::Word(b"8080".to_vec()));
    assert_eq!(tokens[1].span().range(), ByteRange::new(7, 11));
    assert_eq!(tokens[2].kind(), &TokenKind::Semicolon);
    assert_eq!(tokens[2].span().range(), ByteRange::new(11, 12));
}

#[test]
fn spans_count_source_bytes_instead_of_characters() {
    let source = source("set café;");
    let report = lex(&source);

    assert!(report.diagnostics().is_empty());
    assert_eq!(
        report.value()[1].kind(),
        &TokenKind::Word("café".as_bytes().to_vec())
    );
    assert_eq!(report.value()[1].span().range(), ByteRange::new(4, 9));
    assert_eq!(report.value()[2].span().range(), ByteRange::new(9, 10));
}

#[test]
fn comments_start_only_at_token_boundaries() {
    let source = source("# first\nset value#part; # trailing\nset \"# quoted\";\n");
    let report = lex(&source);

    assert!(report.diagnostics().is_empty());
    assert_eq!(
        words(report.value()),
        [
            b"set".as_slice(),
            b"value#part".as_slice(),
            b"set".as_slice(),
            b"# quoted".as_slice()
        ]
    );
}

#[test]
fn quoted_words_and_native_escapes_are_decoded() {
    let source = source(r#"set "a b\t\n\r\"\'\\\q"; set 'single\'quote'; set foo\;bar;"#);
    let report = lex(&source);

    assert!(report.diagnostics().is_empty());
    assert_eq!(
        words(report.value()),
        [
            b"set".as_slice(),
            b"a b\t\n\r\"'\\\\q".as_slice(),
            b"set".as_slice(),
            b"single'quote".as_slice(),
            b"set".as_slice(),
            b"foo\\;bar".as_slice()
        ]
    );
}

#[test]
fn braced_variables_do_not_open_structural_blocks() {
    let source = source("proxy_pass http://${backend}/path; map $x${suffix} { nested on; }");
    let report = lex(&source);

    assert!(report.diagnostics().is_empty());
    assert_eq!(
        words(report.value()),
        [
            b"proxy_pass".as_slice(),
            b"http://${backend}/path".as_slice(),
            b"map".as_slice(),
            b"$x${suffix}".as_slice(),
            b"nested".as_slice(),
            b"on".as_slice()
        ]
    );
    assert_eq!(
        report
            .value()
            .iter()
            .filter(|token| token.kind() == &TokenKind::OpenBrace)
            .count(),
        1
    );
}

#[test]
fn braces_follow_nginx_word_boundaries() {
    let source = source("block{ set value}; }");
    let report = lex(&source);

    assert!(report.diagnostics().is_empty());
    assert_eq!(
        words(report.value()),
        [b"block".as_slice(), b"set".as_slice(), b"value}".as_slice()]
    );
    assert_eq!(
        report
            .value()
            .iter()
            .filter(|token| token.kind() == &TokenKind::OpenBrace)
            .count(),
        1
    );
    assert_eq!(
        report
            .value()
            .iter()
            .filter(|token| token.kind() == &TokenKind::CloseBrace)
            .count(),
        1
    );
}

#[test]
fn quoted_words_require_a_following_token_boundary() {
    let source = source("set \"ok\"suffix; next fine;");
    let report = lex(&source);

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].code(), E_SYNTAX);
    assert_eq!(report.diagnostics()[0].stage(), DiagnosticStage::Lex);
    assert_eq!(
        report.diagnostics()[0]
            .primary_span()
            .expect("located diagnostic")
            .range(),
        ByteRange::new(8, 9)
    );
    assert_eq!(
        words(report.value()),
        [
            b"set".as_slice(),
            b"ok".as_slice(),
            b"next".as_slice(),
            b"fine".as_slice()
        ]
    );
}

#[test]
fn independent_quoted_boundary_errors_are_collected() {
    let source = source("set \"a\"x; set \"b\"y;");
    let report = lex(&source);

    assert_eq!(report.diagnostics().len(), 2);
    assert_eq!(
        report
            .diagnostics()
            .iter()
            .map(|diagnostic| {
                diagnostic
                    .primary_span()
                    .expect("located diagnostic")
                    .range()
                    .start()
            })
            .collect::<Vec<_>>(),
        [7, 17]
    );
    assert_eq!(
        words(report.value()),
        [
            b"set".as_slice(),
            b"a".as_slice(),
            b"set".as_slice(),
            b"b".as_slice()
        ]
    );
}

#[test]
fn unterminated_quotes_and_escapes_are_strict_errors() {
    let quote_source = source("set \"bad");
    let quote_report = lex(&quote_source);
    assert_eq!(quote_report.diagnostics().len(), 1);
    assert_eq!(
        quote_report.diagnostics()[0]
            .primary_span()
            .expect("located quote")
            .range(),
        ByteRange::new(4, 8)
    );

    let escape_source = source("set bad\\");
    let escape_report = lex(&escape_source);
    assert_eq!(escape_report.diagnostics().len(), 1);
    assert_eq!(
        escape_report.diagnostics()[0]
            .primary_span()
            .expect("located escape")
            .range(),
        ByteRange::new(7, 8)
    );
}

#[test]
fn non_utf8_words_are_preserved_as_bytes() {
    let source = byte_source(vec![b's', b'e', b't', b' ', 0xff, 0xfe, b';']);
    let report = lex(&source);

    assert!(report.diagnostics().is_empty());
    assert_eq!(words(report.value()), [b"set".as_slice(), &[0xff, 0xfe]]);
    assert_eq!(
        source.slice(report.value()[1].span().range()),
        Some([0xff, 0xfe].as_slice())
    );
}

#[test]
fn non_utf8_comments_are_ignored_structurally() {
    let source = byte_source(vec![
        b'#', 0xff, 0xfe, b'\n', b's', b'e', b't', b' ', b'o', b'k', b';',
    ]);
    let report = lex(&source);

    assert!(report.diagnostics().is_empty());
    assert_eq!(words(report.value()), [b"set".as_slice(), b"ok".as_slice()]);
}

#[test]
fn non_utf8_quoted_words_are_normalized_without_losing_raw_lexemes() {
    let source = byte_source(vec![
        b's', b'e', b't', b' ', b'"', 0xff, b'\\', b'n', 0xfe, b'"', b';',
    ]);
    let report = lex(&source);
    let quoted = &report.value()[1];

    assert!(report.diagnostics().is_empty());
    assert_eq!(quoted.kind(), &TokenKind::Word(vec![0xff, b'\n', 0xfe]));
    assert_eq!(quoted.span().range(), ByteRange::new(4, 10));
    assert_eq!(
        source.slice(quoted.span().range()),
        Some([b'"', 0xff, b'\\', b'n', 0xfe, b'"'].as_slice())
    );
}

#[test]
fn quote_boundary_recovery_advances_over_non_utf8_bytes() {
    let source = byte_source(vec![
        b's', b'e', b't', b' ', b'"', b'o', b'k', b'"', 0xff, 0xfe, b';', b' ', b'n', b'e', b'x',
        b't', b' ', b'f', b'i', b'n', b'e', b';',
    ]);
    let report = lex(&source);

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].code(), E_SYNTAX);
    assert_eq!(
        report.diagnostics()[0]
            .primary_span()
            .expect("located boundary byte")
            .range(),
        ByteRange::new(8, 9)
    );
    assert_eq!(
        words(report.value()),
        [
            b"set".as_slice(),
            b"ok".as_slice(),
            b"next".as_slice(),
            b"fine".as_slice()
        ]
    );
}

#[test]
fn every_token_span_is_within_its_source() {
    let source = source("http { server { listen 80; } }\n");
    let report = lex(&source);

    assert!(report.diagnostics().is_empty());
    for token in report.value() {
        assert_eq!(token.span().source(), source.id());
        assert!(source.slice(token.span().range()).is_some());
    }
}

fn source(contents: &str) -> SourceFile {
    SourceFile::new(SourceId::new(1), "nginx.conf", contents.as_bytes())
}

fn byte_source(contents: Vec<u8>) -> SourceFile {
    SourceFile::new(SourceId::new(1), "nginx.conf", contents)
}

fn words(tokens: &[oxiroute_import::nginx::Token]) -> Vec<&[u8]> {
    tokens
        .iter()
        .filter_map(|token| match token.kind() {
            TokenKind::Word(value) => Some(value.as_slice()),
            TokenKind::Semicolon | TokenKind::OpenBrace | TokenKind::CloseBrace => None,
        })
        .collect()
}
