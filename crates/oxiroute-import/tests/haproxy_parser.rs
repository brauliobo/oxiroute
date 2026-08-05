use std::path::PathBuf;

use oxiroute_import::{
    ByteRange, DiagnosticStage, SourceFile, SourceId,
    haproxy::{
        E_CONDITIONAL_PREPROCESSING, E_ENVIRONMENT_EXPANSION, LoadedSource, SectionKind, parse,
        parse_sources,
    },
};

#[test]
fn all_known_section_starters_terminate_the_preceding_section() {
    let sections = [
        ("global", SectionKind::Global),
        ("defaults named", SectionKind::Defaults),
        ("frontend named", SectionKind::Frontend),
        ("backend named", SectionKind::Backend),
        ("listen named", SectionKind::Listen),
        ("userlist named", SectionKind::Userlist),
        ("peers named", SectionKind::Peers),
        ("mailers named", SectionKind::Mailers),
        ("namespace_list", SectionKind::NamespaceList),
        ("traces named", SectionKind::Traces),
        ("resolvers named", SectionKind::Resolvers),
        ("cache named", SectionKind::Cache),
        ("fcgi-app named", SectionKind::FcgiApp),
        ("ring named", SectionKind::Ring),
        ("log-forward named", SectionKind::LogForward),
        ("log-profile named", SectionKind::LogProfile),
        ("http-errors named", SectionKind::HttpErrors),
        ("crt-store named", SectionKind::CrtStore),
        ("acme named", SectionKind::Acme),
        ("healthcheck named", SectionKind::Healthcheck),
        ("program named", SectionKind::Program),
    ];
    let mut contents = String::new();
    for (header, _) in sections {
        contents.push_str(header);
        contents.push_str("\n  marker value\n");
    }
    let source = source(contents.as_bytes());
    let report = parse(&source);

    assert!(report.diagnostics().is_empty());
    assert_eq!(report.value().sections.len(), sections.len());
    assert_eq!(
        report
            .value()
            .sections
            .iter()
            .map(|section| section.kind)
            .collect::<Vec<_>>(),
        sections.map(|(_, kind)| kind)
    );
    assert!(
        report
            .value()
            .sections
            .iter()
            .all(|section| section.directives.len() == 1
                && section.directives[0].name.value == b"marker")
    );
}

#[test]
fn unresolved_environment_references_block_activation_but_retain_raw_ast() {
    let source = source(b"frontend \"$NAME\"\n  bind \"${ADDRESS-:80}\"\n");
    let report = parse(&source);

    assert!(report.has_errors());
    assert_eq!(report.diagnostics().len(), 2);
    assert!(report.diagnostics().iter().all(|diagnostic| {
        diagnostic.code() == E_ENVIRONMENT_EXPANSION
            && diagnostic.stage() == DiagnosticStage::Resolve
            && diagnostic.primary_span().is_some()
    }));
    assert_eq!(report.value().sections.len(), 1);
    assert_eq!(report.value().sections[0].kind, SectionKind::Frontend);
    assert_eq!(
        report.value().sections[0].header.arguments[0].value,
        b"$NAME"
    );
    assert_eq!(
        report.value().sections[0].directives[0].arguments[0].value,
        b"${ADDRESS-:80}"
    );
}

#[test]
fn conditional_preprocessing_blocks_raw_section_classification() {
    let source = source(
        b".if defined(ENABLED)\nfrontend enabled\n.elif defined(CHECKS)\nhealthcheck checks\n.else\nbackend fallback\n.endif\n",
    );
    let report = parse(&source);

    assert!(report.has_errors());
    assert_eq!(
        report
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.code() == E_CONDITIONAL_PREPROCESSING)
            .count(),
        4
    );
    assert!(
        report
            .diagnostics()
            .iter()
            .all(|diagnostic| diagnostic.stage() == DiagnosticStage::Resolve)
    );
    assert_eq!(
        report
            .value()
            .sections
            .iter()
            .map(|section| section.kind)
            .collect::<Vec<_>>(),
        [
            SectionKind::Frontend,
            SectionKind::Healthcheck,
            SectionKind::Backend,
        ]
    );
    assert_eq!(report.value().preamble[0].name.value, b".if");
    assert_eq!(
        report.value().sections[0].directives[0].name.value,
        b".elif"
    );
    assert_eq!(
        report.value().sections[1].directives[0].name.value,
        b".else"
    );
    assert_eq!(
        report.value().sections[2].directives[0].name.value,
        b".endif"
    );
}

#[test]
fn unsupported_sections_do_not_leak_directives_into_proxies() {
    let source = source(
        b"frontend public\n  bind :80\ncache objects\n  total-max-size 32\nbackend app\n  server app1 127.0.0.1:8080\n",
    );
    let report = parse(&source);
    let sections = &report.value().sections;

    assert!(report.diagnostics().is_empty());
    assert_eq!(sections.len(), 3);
    assert_eq!(sections[0].kind, SectionKind::Frontend);
    assert_eq!(sections[0].directives[0].name.value, b"bind");
    assert_eq!(sections[1].kind, SectionKind::Cache);
    assert_eq!(sections[1].directives[0].name.value, b"total-max-size");
    assert_eq!(sections[2].kind, SectionKind::Backend);
    assert_eq!(sections[2].directives[0].name.value, b"server");
}

#[test]
fn directive_comment_line_and_section_spans_are_exact() {
    let source =
        source(b"# lead\r\nfrontend public # header\r\n\tbind :80 # listener\r\nbackend app\n");
    let report = parse(&source);
    let document = report.value();

    assert!(report.diagnostics().is_empty());
    assert_eq!(document.lines.len(), 4);
    assert_eq!(document.span, source.full_span());
    assert_eq!(document.sections[0].span.range(), ByteRange::new(8, 56));
    assert_eq!(
        document.sections[0].header.span.range(),
        ByteRange::new(8, 23)
    );
    assert_eq!(
        document.sections[0]
            .header
            .comment
            .expect("header comment")
            .range(),
        ByteRange::new(24, 32)
    );
    assert_eq!(
        document.sections[0].directives[0].line_span.range(),
        ByteRange::new(34, 56)
    );
    assert_eq!(
        document.sections[1].span.range(),
        ByteRange::new(56, source.len())
    );
}

#[test]
fn parser_retains_ordered_acl_conjunction_words_and_source_span() {
    let source = source(
        b"frontend public\n  acl app_host hdr(host) -i api.example.test\n  acl app_path path_beg /api\n  use_backend app if app_host app_path\n",
    );
    let report = parse(&source);
    let directive = &report.value().sections[0].directives[2];

    assert!(report.diagnostics().is_empty());
    assert_eq!(directive.name.value, b"use_backend");
    assert_eq!(
        directive
            .arguments
            .iter()
            .map(|word| word.value.as_slice())
            .collect::<Vec<_>>(),
        [
            b"app".as_slice(),
            b"if".as_slice(),
            b"app_host".as_slice(),
            b"app_path".as_slice(),
        ]
    );
    assert_eq!(
        &source.bytes()[directive.span.range().start()..directive.span.range().end()],
        b"use_backend app if app_host app_path"
    );
}

#[test]
fn parser_retains_http_check_send_tokens_and_source_span() {
    let source = source(
        b"backend app\n  http-check send meth GET uri /healthz ver HTTP/1.1 hdr Host app.internal\n",
    );
    let report = parse(&source);
    let directive = &report.value().sections[0].directives[0];

    assert!(report.diagnostics().is_empty());
    assert_eq!(directive.name.value, b"http-check");
    assert_eq!(
        directive
            .arguments
            .iter()
            .map(|word| word.value.as_slice())
            .collect::<Vec<_>>(),
        [
            b"send".as_slice(),
            b"meth".as_slice(),
            b"GET".as_slice(),
            b"uri".as_slice(),
            b"/healthz".as_slice(),
            b"ver".as_slice(),
            b"HTTP/1.1".as_slice(),
            b"hdr".as_slice(),
            b"Host".as_slice(),
            b"app.internal".as_slice(),
        ]
    );
    assert_eq!(
        &source.bytes()[directive.span.range().start()..directive.span.range().end()],
        b"http-check send meth GET uri /healthz ver HTTP/1.1 hdr Host app.internal"
    );
}

#[test]
fn each_file_ends_its_section_in_multi_file_parsing() {
    let first = loaded(0, 0, "first.cfg", b"frontend public\n  mode http\n");
    let second = loaded(
        1,
        1,
        "second.cfg",
        b"  bind :80\nbackend app\n  server app1 127.0.0.1:8080\n",
    );
    let report = parse_sources(&[first, second]);
    let files = &report.value().files;

    assert!(report.diagnostics().is_empty());
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].document.sections.len(), 1);
    assert_eq!(files[0].document.sections[0].kind, SectionKind::Frontend);
    assert_eq!(files[0].document.sections[0].directives.len(), 1);
    assert_eq!(files[1].document.preamble.len(), 1);
    assert_eq!(files[1].document.preamble[0].name.value, b"bind");
    assert_eq!(files[1].document.sections.len(), 1);
    assert_eq!(files[1].document.sections[0].kind, SectionKind::Backend);
}

#[test]
fn non_utf8_directives_parse_as_bytes() {
    let source = source(&[
        b'f', b'r', b'o', b'n', b't', b'e', b'n', b'd', b' ', 0xff, b'\n', b' ', b' ', 0xfe, b' ',
        b'"', 0xfd, b'"', b'\n',
    ]);
    let first = parse(&source);
    let second = parse(&source);

    assert_eq!(first, second);
    assert!(first.diagnostics().is_empty());
    assert_eq!(first.value().sections[0].header.arguments[0].value, [0xff]);
    assert_eq!(first.value().sections[0].directives[0].name.value, [0xfe]);
    assert_eq!(
        first.value().sections[0].directives[0].arguments[0].value,
        [0xfd]
    );
}

fn source(contents: &[u8]) -> SourceFile {
    SourceFile::new(SourceId::new(1), "haproxy.cfg", contents)
}

fn loaded(root_ordinal: usize, file_ordinal: usize, path: &str, contents: &[u8]) -> LoadedSource {
    let path = PathBuf::from(path);
    LoadedSource {
        root_ordinal,
        file_ordinal,
        source: SourceFile::from_path(
            SourceId::new(u32::try_from(file_ordinal).expect("test source id")),
            path.clone(),
            contents,
        ),
        path,
    }
}
