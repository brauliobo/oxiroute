use oxiroute_import::{
    SourceFile, SourceId,
    nginx::{Directive, parse},
};

#[test]
fn record_path_escaped_space_preserves_the_backslash() {
    let source = SourceFile::new(
        SourceId::new(1),
        "nginx.conf",
        br"rtmp { server { application live { record_path /var/media\ files; } } }".as_slice(),
    );
    let report = parse(&source);

    assert!(report.diagnostics().is_empty());
    let rtmp = child(&report.value().directives, b"rtmp");
    let server = child(rtmp.children.as_deref().expect("rtmp block"), b"server");
    let application = child(
        server.children.as_deref().expect("server block"),
        b"application",
    );
    let record_path = child(
        application.children.as_deref().expect("application block"),
        b"record_path",
    );

    assert_eq!(record_path.arguments.len(), 1);
    assert_eq!(record_path.arguments[0].value, br"/var/media\ files");
    assert_eq!(
        source.slice(record_path.arguments[0].span.range()),
        Some(br"/var/media\ files".as_slice())
    );
}

fn child<'a>(directives: &'a [Directive], name: &[u8]) -> &'a Directive {
    directives
        .iter()
        .find(|directive| directive.name.value == name)
        .expect("named directive")
}
