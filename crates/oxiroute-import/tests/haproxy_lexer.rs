use std::fmt::Write as _;

use oxiroute_import::{
    ByteRange, DiagnosticStage, E_SOURCE_LIMIT, SourceFile, SourceId,
    haproxy::{E_SYNTAX, LineEnding, MAX_WORDS_PER_LINE, lex},
};

#[test]
fn physical_lines_preserve_comments_tabs_and_mixed_line_endings() {
    let source = source(
        b"\tfrontend\tpublic#comment\r\n  bind\t\":80 #literal\"\n# only\rcache objects\nbackend app\n",
    );
    let report = lex(&source);
    let lines = report.value();

    assert!(report.diagnostics().is_empty());
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0].ending, LineEnding::CrLf);
    assert_eq!(lines[0].span.range(), ByteRange::new(0, 26));
    assert_eq!(
        values(&lines[0].words),
        [b"frontend".as_slice(), b"public".as_slice()]
    );
    assert_eq!(
        lines[0].comment.expect("trailing comment").range(),
        ByteRange::new(16, 24)
    );
    assert_eq!(lines[1].ending, LineEnding::Lf);
    assert_eq!(
        values(&lines[1].words),
        [b"bind".as_slice(), b":80 #literal".as_slice()]
    );
    assert!(lines[1].comment.is_none());
    assert_eq!(lines[2].ending, LineEnding::Lf);
    assert!(lines[2].words.is_empty());
    assert_eq!(
        source.slice(lines[2].comment.expect("comment-only line").range()),
        Some(b"# only".as_slice())
    );
    assert_eq!(lines[2].span.range(), ByteRange::new(48, 69));
    assert_eq!(lines[3].ending, LineEnding::Lf);
    assert_eq!(
        values(&lines[3].words),
        [b"backend".as_slice(), b"app".as_slice()]
    );
}

#[test]
fn final_unterminated_lines_and_nul_bytes_are_blocking_errors() {
    let unterminated = source(b"global\nfrontend final");
    let unterminated_report = lex(&unterminated);

    assert_eq!(unterminated_report.value().len(), 2);
    assert_eq!(unterminated_report.value()[1].ending, LineEnding::None);
    assert_eq!(unterminated_report.diagnostics().len(), 1);
    assert_eq!(unterminated_report.diagnostics()[0].code(), E_SYNTAX);
    assert_eq!(
        unterminated_report.diagnostics()[0]
            .primary_span()
            .expect("unterminated line span")
            .range(),
        ByteRange::new(7, 21)
    );

    let nul = source(b"global\nfront\0end public\nbackend ignored\n");
    let nul_report = lex(&nul);
    assert_eq!(nul_report.value().len(), 1);
    assert_eq!(nul_report.value()[0].words[0].value, b"global");
    assert_eq!(nul_report.diagnostics().len(), 1);
    assert_eq!(nul_report.diagnostics()[0].code(), E_SYNTAX);
    assert_eq!(
        nul_report.diagnostics()[0]
            .primary_span()
            .expect("NUL span")
            .range(),
        ByteRange::new(12, 13)
    );
}

#[test]
fn empty_quoted_arguments_and_nul_escapes_are_rejected() {
    let source = source(b"set \"\"\nset ''\nset \\x00\nfrontend valid\n");
    let report = lex(&source);

    assert_eq!(report.diagnostics().len(), 3);
    assert!(
        report
            .diagnostics()
            .iter()
            .all(|diagnostic| diagnostic.code() == E_SYNTAX)
    );
    assert_eq!(report.value().len(), 1);
    assert_eq!(report.value()[0].words[0].value, b"frontend");
}

#[test]
fn native_per_line_word_limit_includes_the_directive_keyword() {
    let boundary = line_with_words(MAX_WORDS_PER_LINE);
    let boundary_report = lex(&source(boundary.as_bytes()));
    assert!(boundary_report.diagnostics().is_empty());
    assert_eq!(boundary_report.value()[0].words.len(), MAX_WORDS_PER_LINE);

    let over = line_with_words(MAX_WORDS_PER_LINE + 1);
    let first = lex(&source(over.as_bytes()));
    let second = lex(&source(over.as_bytes()));
    assert_eq!(first, second);
    assert!(first.value().is_empty());
    assert_eq!(first.diagnostics().len(), 1);
    assert_eq!(first.diagnostics()[0].code(), E_SOURCE_LIMIT);
    assert_eq!(first.diagnostics()[0].stage(), DiagnosticStage::Lex);
}

#[test]
fn weak_and_strong_quotes_follow_haproxy_escape_rules() {
    let source = source(
        br#"set "weak $USER ${PORT-80} \$ \# \\ \x41 \n" 'strong\n#$USER' unquoted\ value\#hash\q
"#,
    );
    let report = lex(&source);
    let words = &report.value()[0].words;

    assert!(report.diagnostics().is_empty());
    assert_eq!(words.len(), 4);
    assert_eq!(words[0].value, b"set");
    assert_eq!(words[1].value, b"weak $USER ${PORT-80} $ # \\ A \n");
    assert_eq!(words[1].environment_references.len(), 2);
    assert!(words[1].has_environment_references());
    assert_eq!(
        words[1]
            .environment_references
            .iter()
            .map(|span| source.slice(span.range()).expect("reference bytes"))
            .collect::<Vec<_>>(),
        [b"$USER".as_slice(), b"${PORT-80}".as_slice()]
    );
    assert_eq!(words[2].value, br"strong\n#$USER");
    assert!(!words[2].has_environment_references());
    assert_eq!(words[3].value, br"unquoted value#hash\q");
}

#[test]
fn quote_and_hex_escape_failures_are_located_and_recover_by_line() {
    let source = source(b"set \"weak\nset 'strong\nset \\x4\nset \\xGG\nfrontend valid\n");
    let first = lex(&source);
    let second = lex(&source);

    assert_eq!(first, second);
    assert_eq!(first.diagnostics().len(), 4);
    assert!(
        first
            .diagnostics()
            .iter()
            .all(|diagnostic| diagnostic.code() == E_SYNTAX)
    );
    assert!(
        first
            .diagnostics()
            .iter()
            .all(|diagnostic| diagnostic.stage() == DiagnosticStage::Lex)
    );
    assert_eq!(
        first
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic
                .primary_span()
                .expect("located syntax error")
                .range()
                .start())
            .collect::<Vec<_>>(),
        [4, 14, 26, 34]
    );
    assert_eq!(first.value().len(), 1);
    assert_eq!(
        values(&first.value()[0].words),
        [b"frontend".as_slice(), b"valid".as_slice()]
    );
}

#[test]
fn escaped_environment_markers_are_not_references() {
    let source = source(
        br#"set "\$LITERAL" "$ACTIVE" '$STRONG' $UNQUOTED
"#,
    );
    let report = lex(&source);
    let words = &report.value()[0].words;

    assert!(report.diagnostics().is_empty());
    assert!(!words[1].has_environment_references());
    assert!(words[2].has_environment_references());
    assert!(!words[3].has_environment_references());
    assert!(!words[4].has_environment_references());
}

#[test]
fn only_native_dot_prefixed_environment_builtins_are_accepted() {
    let source =
        source(b"set \"$.LINE\" \"${.FILE}\" \"$.SECTION\"\nset \"$.OTHER\"\nfrontend valid\n");
    let report = lex(&source);

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].code(), E_SYNTAX);
    assert_eq!(report.value().len(), 2);
    assert_eq!(report.value()[0].words.len(), 4);
    assert_eq!(
        report.value()[0]
            .words
            .iter()
            .map(|word| word.environment_references.len())
            .collect::<Vec<_>>(),
        [0, 1, 1, 1]
    );
    assert_eq!(report.value()[1].words[0].value, b"frontend");
}

#[test]
fn non_utf8_words_and_comments_remain_available_through_exact_spans() {
    let source = source(&[
        b's', b'e', b't', b' ', 0xff, b'\\', b' ', 0xfe, b' ', b'#', 0xfd, b'\n',
    ]);
    let report = lex(&source);
    let line = &report.value()[0];

    assert!(report.diagnostics().is_empty());
    assert_eq!(line.words[1].value, [0xff, b' ', 0xfe]);
    assert_eq!(
        source.slice(line.words[1].span.range()),
        Some([0xff, b'\\', b' ', 0xfe].as_slice())
    );
    assert_eq!(
        source.slice(line.comment.expect("byte comment").range()),
        Some([b'#', 0xfd].as_slice())
    );
}

fn source(contents: &[u8]) -> SourceFile {
    SourceFile::new(SourceId::new(1), "haproxy.cfg", contents)
}

fn values(words: &[oxiroute_import::haproxy::Word]) -> Vec<&[u8]> {
    words.iter().map(|word| word.value.as_slice()).collect()
}

fn line_with_words(word_count: usize) -> String {
    let mut line = String::from("keyword");
    for index in 0..word_count - 1 {
        write!(line, " arg{index}").expect("write test argument");
    }
    line.push('\n');
    line
}
