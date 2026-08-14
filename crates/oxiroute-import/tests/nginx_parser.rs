use oxiroute_import::{
    ByteRange, SourceFile, SourceId,
    nginx::{Directive, parse},
};

#[test]
fn parses_statement_and_block_directives_with_full_spans() {
    let source = source("http { server { listen 80; } } daemon off;");
    let report = parse(&source);
    let document = report.value();

    assert!(report.diagnostics().is_empty());
    assert_eq!(document.span.range(), ByteRange::new(0, 42));
    assert_eq!(document.directives.len(), 2);

    let http = &document.directives[0];
    assert_eq!(http.name.value, b"http");
    assert_eq!(http.name.span.range(), ByteRange::new(0, 4));
    assert_eq!(http.span.range(), ByteRange::new(0, 30));

    let server = &http.children.as_ref().expect("http block")[0];
    assert_eq!(server.name.value, b"server");
    assert_eq!(server.span.range(), ByteRange::new(7, 28));

    let listen = &server.children.as_ref().expect("server block")[0];
    assert_eq!(listen.name.value, b"listen");
    assert_eq!(listen.arguments[0].value, b"80");
    assert_eq!(listen.arguments[0].span.range(), ByteRange::new(23, 25));
    assert_eq!(listen.span.range(), ByteRange::new(16, 26));
    assert!(listen.children.is_none());

    let daemon = &document.directives[1];
    assert_eq!(daemon.name.value, b"daemon");
    assert_eq!(daemon.span.range(), ByteRange::new(31, 42));
}

#[test]
fn parses_nested_rtmp_and_http_configuration() {
    let source = source(
        r#"
rtmp_auto_push on;
rtmp {
  server {
    listen 1935 proxy_protocol;
    application live {
      live on;
      record_path "/var/media files";
      push rtmp://backup.example/live app=archive;
      recorder archive { record all manual; }
    }
  }
}
http { server { location /stat { rtmp_stat all; } } }
"#,
    );
    let report = parse(&source);

    assert!(report.diagnostics().is_empty());
    assert_eq!(report.value().directives.len(), 3);
    let rtmp = &report.value().directives[1];
    let server = child(rtmp, b"server");
    let application = child(server, b"application");
    assert_eq!(application.arguments[0].value, b"live");
    assert_eq!(
        child(application, b"record_path").arguments[0].value,
        b"/var/media files"
    );
    assert_eq!(
        child(application, b"recorder").arguments[0].value,
        b"archive"
    );
    let http = &report.value().directives[2];
    assert_eq!(
        child(child(child(http, b"server"), b"location"), b"rtmp_stat").arguments[0].value,
        b"all"
    );
}

#[test]
fn parses_keepalive_and_cookie_flag_arguments_without_normalizing_them() {
    let source = source(
        "http { keepalive_timeout 65s; proxy_cookie_flags session secure httponly samesite=lax; }",
    );
    let report = parse(&source);
    let directives = &report.value().directives[0]
        .children
        .as_ref()
        .expect("http block");

    assert!(report.diagnostics().is_empty());
    assert_eq!(directives[0].name.value, b"keepalive_timeout");
    assert_eq!(directives[0].arguments[0].value, b"65s");
    assert_eq!(directives[1].name.value, b"proxy_cookie_flags");
    assert_eq!(
        directives[1]
            .arguments
            .iter()
            .map(|argument| argument.value.as_slice())
            .collect::<Vec<_>>(),
        [
            b"session".as_slice(),
            b"secure".as_slice(),
            b"httponly".as_slice(),
            b"samesite=lax".as_slice(),
        ]
    );
}

#[test]
fn semicolons_and_braces_define_directive_shape() {
    let source = source("empty {} statement value; outer { inner one; }");
    let report = parse(&source);
    let directives = &report.value().directives;

    assert!(report.diagnostics().is_empty());
    assert_eq!(directives.len(), 3);
    assert_eq!(directives[0].children.as_deref(), Some([].as_slice()));
    assert!(directives[1].children.is_none());
    assert_eq!(
        directives[2].children.as_ref().expect("outer block")[0]
            .name
            .value,
        b"inner"
    );
}

#[test]
fn independent_structural_errors_are_collected_in_source_order() {
    let source = source("; good one; } { also ok; broken");
    let first = parse(&source);
    let second = parse(&source);

    assert_eq!(first.diagnostics(), second.diagnostics());
    assert_eq!(first.diagnostics().len(), 4);
    assert_eq!(
        first
            .diagnostics()
            .iter()
            .map(|diagnostic| {
                diagnostic
                    .primary_span()
                    .expect("parser diagnostics are located")
                    .range()
                    .start()
            })
            .collect::<Vec<_>>(),
        [0, 12, 14, 31]
    );
    assert_eq!(
        first
            .value()
            .directives
            .iter()
            .map(|directive| directive.name.value.as_slice())
            .collect::<Vec<_>>(),
        [b"good".as_slice(), b"also".as_slice()]
    );
}

#[test]
fn an_unclosed_block_retains_its_partial_ast() {
    let source = source("outer { inner on;");
    let report = parse(&source);
    let outer = &report.value().directives[0];

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(outer.name.value, b"outer");
    assert_eq!(outer.span.range(), ByteRange::new(0, source.len()));
    assert_eq!(
        outer.children.as_ref().expect("partial block")[0]
            .name
            .value,
        b"inner"
    );
}

#[test]
fn every_ast_span_is_within_the_single_source() {
    let source = source("http { server { listen 80; server_name example.test; } }");
    let report = parse(&source);

    assert!(report.diagnostics().is_empty());
    assert_eq!(report.value().span, source.full_span());
    for directive in &report.value().directives {
        assert_directive_bounds(&source, directive);
    }
}

#[test]
fn non_utf8_sources_parse_deterministically_as_bytes() {
    let source = byte_source(vec![
        b'#', 0xff, b'\n', 0xfe, b' ', b'{', b' ', 0xfd, b' ', b'"', 0xfc, b'\\', b'n', b'"', b';',
        b' ', b'}',
    ]);
    let first = parse(&source);
    let second = parse(&source);

    assert_eq!(first, second);
    assert!(first.diagnostics().is_empty());

    let outer = &first.value().directives[0];
    assert_eq!(outer.name.value, [0xfe]);
    assert_eq!(
        source.slice(outer.name.span.range()),
        Some([0xfe].as_slice())
    );

    let child = &outer.children.as_ref().expect("byte-valued block")[0];
    assert_eq!(child.name.value, [0xfd]);
    assert_eq!(child.arguments[0].value, [0xfc, b'\n']);
    assert_eq!(
        source.slice(child.arguments[0].span.range()),
        Some([b'"', 0xfc, b'\\', b'n', b'"'].as_slice())
    );
}

fn assert_directive_bounds(source: &SourceFile, directive: &Directive) {
    assert_eq!(directive.span.source(), source.id());
    assert!(source.slice(directive.span.range()).is_some());
    assert!(source.slice(directive.name.span.range()).is_some());
    for argument in &directive.arguments {
        assert_eq!(argument.span.source(), source.id());
        assert!(source.slice(argument.span.range()).is_some());
    }
    if let Some(children) = &directive.children {
        for child in children {
            assert_directive_bounds(source, child);
        }
    }
}

fn child<'a>(parent: &'a Directive, name: &[u8]) -> &'a Directive {
    parent
        .children
        .as_ref()
        .expect("block")
        .iter()
        .find(|directive| directive.name.value == name)
        .expect("named child")
}

fn source(contents: &str) -> SourceFile {
    SourceFile::new(SourceId::new(1), "nginx.conf", contents.as_bytes())
}

fn byte_source(contents: Vec<u8>) -> SourceFile {
    SourceFile::new(SourceId::new(1), "nginx.conf", contents)
}
