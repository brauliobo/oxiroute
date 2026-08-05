#![cfg(unix)]

use std::{fs, net::TcpListener, path::Path};

use oxiroute_config_source::{
    ConfigFormat, ConfigSourceError, decode_value, resolve_source, resolve_source_with_format,
};
use tempfile::tempdir;

#[test]
fn concise_kdl_imports_relative_nginx_and_ordered_haproxy_roots() {
    let directory = native_fixture_directory();
    let source_path = directory.path().join("host.kdl");
    let source = br#"
nginx_server "nginx.conf" { root_prefix "." }
haproxy_server "frontend.cfg" "backend.cfg" {
  node_ip "10.0.0.11"
  gpu1_defined #false
}
"#;

    let resolved = resolve_source(&source_path, source).expect("resolved native KDL");

    assert_eq!(resolved.format, ConfigFormat::Kdl);
    assert_eq!(resolved.config.version, 1);
    assert_eq!(resolved.config.listeners.len(), 2);
    assert!(resolved.compositional);
    assert_eq!(resolved.dependencies.len(), 4);
    assert_eq!(
        resolved.dependencies[0],
        fs::canonicalize(directory.path().join("nginx.conf")).unwrap()
    );
    assert_eq!(resolved.dependencies[1], directory.path());
    assert_eq!(
        resolved.dependencies[2],
        directory.path().join("frontend.cfg")
    );
    assert_eq!(
        resolved.dependencies[3],
        directory.path().join("backend.cfg")
    );
}

#[test]
fn concise_kdl_imports_a_complete_squid_forward_proxy_root() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../oxiroute-import/tests/fixtures/squid/hostrouter-sanitized.conf");
    let source_path = root.parent().unwrap().join("host.kdl");
    let source = format!("squid_server {root:?} {{ externalize_cache #true }}\n");
    let resolved = resolve_source(&source_path, source.as_bytes()).expect("resolved Squid KDL");
    assert_eq!(resolved.config.listeners.len(), 1);
    assert_eq!(resolved.config.forward_proxy_services.len(), 1);
    assert!(resolved.compositional);
    assert_eq!(
        resolved.dependencies,
        [
            fs::canonicalize(&root).unwrap(),
            fs::canonicalize(root.parent().unwrap()).unwrap()
        ]
    );
    assert_eq!(resolved.native_references.len(), 1);
    assert_eq!(
        resolved.native_references[0].evidence.source.product,
        "squid"
    );
    assert!(
        !resolved.native_references[0]
            .evidence
            .candidate
            .provenance
            .is_empty()
    );
    resolve_source(&source_path, resolved.canonical_kdl.as_bytes())
        .expect("rendered Squid candidate round trip");
}

#[test]
fn native_apache_imports_from_kdl_hocon_and_uci() {
    let directory = tempdir().expect("temporary Apache source");
    let root = directory.path().join("httpd.conf");
    fs::write(
        &root,
        b"Listen 127.0.0.1:18080\n<VirtualHost 127.0.0.1:18080>\n  ServerName app.example.test\n  ProxyPass / http://127.0.0.1:9000/\n</VirtualHost>\n",
    )
    .expect("Apache source");
    let path = root.to_str().expect("UTF-8 Apache path");
    let quoted = serde_json::to_string(path).expect("quoted Apache path");

    for (extension, source) in [
        ("kdl", format!("apache_server {quoted}\n")),
        ("hocon", format!("apache_server = {{ path = {quoted} }}")),
        (
            "uci",
            format!("config apache_server 'site'\n  option path '{path}'\n"),
        ),
    ] {
        let source_path = directory.path().join(format!("host.{extension}"));
        let resolved = resolve_source(&source_path, source.as_bytes())
            .unwrap_or_else(|error| panic!("resolved Apache {extension}: {error}"));
        assert_eq!(resolved.config.listeners.len(), 1, "{extension}");
        assert_eq!(resolved.config.http_services.len(), 1, "{extension}");
        assert!(resolved.compositional, "{extension}");
        assert_eq!(
            resolved.dependencies,
            [
                fs::canonicalize(&root).unwrap(),
                fs::canonicalize(root.parent().unwrap()).unwrap()
            ]
        );
    }
}

#[test]
fn native_apache_reference_retains_include_provenance_in_its_report() {
    let directory = tempdir().expect("temporary Apache source");
    let root = directory.path().join("httpd.conf");
    let included = directory.path().join("site.conf");
    fs::write(&root, b"Include site.conf\n").expect("Apache root");
    fs::write(
        &included,
        b"Listen 127.0.0.1:18087\n<VirtualHost 127.0.0.1:18087>\n  ServerName app.example.test\n  ProxyPass / http://127.0.0.1:9000/\n</VirtualHost>\n",
    )
    .expect("Apache included source");
    let source_path = directory.path().join("host.kdl");
    let source = format!("apache_server {root:?}\n");

    let resolved = resolve_source(&source_path, source.as_bytes()).expect("resolved Apache KDL");
    let reference = &resolved.native_references[0];
    assert_eq!(reference.evidence.source.product, "apache");
    assert!(reference.evidence.candidate.provenance.iter().any(|entry| {
        entry
            .origins
            .iter()
            .any(|origin| !origin.include_stack.is_empty())
    }));
    assert!(
        resolved
            .dependencies
            .iter()
            .any(|dependency| dependency == &fs::canonicalize(&included).unwrap())
    );
}

#[test]
fn hocon_and_uci_import_a_complete_squid_forward_proxy_root() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../oxiroute-import/tests/fixtures/squid/hostrouter-sanitized.conf");
    for (extension, source) in [
        (
            "hocon",
            format!("squid_server = {{ path = {root:?}, externalize_cache = true }}"),
        ),
        (
            "uci",
            format!(
                "config oxiroute 'main'\n  option version '1'\nconfig squid_server 'proxy'\n  option path {root:?}\n  option externalize_cache '1'\n"
            ),
        ),
    ] {
        let source_path = root.parent().unwrap().join(format!("host.{extension}"));
        let resolved = resolve_source(&source_path, source.as_bytes())
            .unwrap_or_else(|error| panic!("resolved Squid {extension}: {error}"));
        assert_eq!(resolved.config.listeners.len(), 1);
        assert_eq!(resolved.config.forward_proxy_services.len(), 1);
        assert!(resolved.compositional);
        assert_eq!(
            resolved.dependencies,
            [
                fs::canonicalize(&root).unwrap(),
                fs::canonicalize(root.parent().unwrap()).unwrap()
            ]
        );
    }
}

#[test]
fn native_squid_references_resolve_the_same_candidate_shape() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../oxiroute-import/tests/fixtures/squid/hostrouter-sanitized.conf");
    let sources = [
        (
            "kdl",
            format!("squid_server {root:?} {{ externalize_cache #true }}\n"),
        ),
        (
            "hocon",
            format!("squid_server = {{ path = {root:?}, externalize_cache = true }}"),
        ),
        (
            "uci",
            format!(
                "config oxiroute 'main'\n  option version '1'\nconfig squid_server 'proxy'\n  option path {root:?}\n  option externalize_cache '1'\n"
            ),
        ),
    ];
    let mut resolved = sources.iter().map(|(extension, source)| {
        resolve_source(
            &root.parent().unwrap().join(format!("native.{extension}")),
            source.as_bytes(),
        )
        .unwrap_or_else(|error| panic!("resolved Squid {extension}: {error}"))
    });
    let first = resolved.next().expect("KDL candidate");
    for candidate in resolved {
        assert_eq!(candidate.config, first.config);
        assert_eq!(candidate.dependencies, first.dependencies);
    }
}

#[test]
fn nginx_native_dependencies_track_glob_parents_and_match_changes() {
    let directory = tempdir().expect("temporary nginx source tree");
    let native = directory.path().join("native");
    let sites = native.join("sites-enabled");
    fs::create_dir_all(&sites).expect("nginx include directory");
    fs::write(
        native.join("nginx.conf"),
        b"events {} http { access_log off; include sites-enabled/*.conf; }",
    )
    .expect("nginx root");
    let first_port = TcpListener::bind("127.0.0.1:0")
        .expect("first listener")
        .local_addr()
        .unwrap()
        .port();
    let first = sites.join("10-first.conf");
    fs::write(
        &first,
        format!("server {{ listen 127.0.0.1:{first_port}; location / {{ return 200 ok; }} }}"),
    )
    .expect("first site");
    let source_path = directory.path().join("host.kdl");
    let source = b"nginx_server \"native/nginx.conf\" { root_prefix \"native\" }\n";

    let resolved = resolve_source(&source_path, source).expect("initial nginx resolution");
    assert!(resolved.dependencies.contains(&sites));
    assert!(
        resolved
            .dependencies
            .contains(&fs::canonicalize(&first).unwrap())
    );

    let renamed = sites.join("20-renamed.conf");
    fs::rename(&first, &renamed).expect("rename included site");
    let resolved = resolve_source(&source_path, source).expect("renamed nginx resolution");
    assert!(resolved.dependencies.contains(&renamed));
    assert!(!resolved.dependencies.contains(&first));

    let second_port = TcpListener::bind("127.0.0.1:0")
        .expect("second listener")
        .local_addr()
        .unwrap()
        .port();
    let added = sites.join("30-added.conf");
    fs::write(
        &added,
        format!("server {{ listen 127.0.0.1:{second_port}; location / {{ return 200 added; }} }}"),
    )
    .expect("added site");
    let resolved = resolve_source(&source_path, source).expect("added nginx resolution");
    assert!(resolved.dependencies.contains(&added));

    fs::remove_file(&renamed).expect("remove renamed site");
    let resolved = resolve_source(&source_path, source).expect("removed nginx resolution");
    assert!(!resolved.dependencies.contains(&renamed));
    assert!(resolved.dependencies.contains(&sites));
}

#[test]
fn native_squid_requires_explicit_cache_externalization() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../oxiroute-import/tests/fixtures/squid/hostrouter-sanitized.conf");
    let source_path = root.parent().unwrap().join("host.kdl");
    let source = format!("squid_server {root:?}\n");

    let error = resolve_source(&source_path, source.as_bytes()).unwrap_err();
    assert!(matches!(error, ConfigSourceError::NativeImport { .. }));
    assert!(error.to_string().contains("E_UNSUPPORTED_FEATURE"));
}

#[test]
fn kdl_native_nodes_are_repeated_but_their_shapes_remain_strict() {
    let invalid = [
        "nginx_server path=\"nginx.conf\"",
        "(source)nginx_server \"nginx.conf\"",
        "nginx_server \"nginx.conf\" { root_prefix \"/\"; root_prefix \"/tmp\"; }",
        "nginx_server \"nginx.conf\" { root_prefix \"/\" { nested \"bad\" } }",
        "haproxy_server 1",
        "apache_server path=\"httpd.conf\"",
        "apache_server \"httpd.conf\" { path \"other.conf\" }",
        "varnish_server path=\"vcl\"",
        "varnish_server \"vcl\" { path \"other.vcl\" }",
    ];
    for source in invalid {
        assert!(
            matches!(
                resolve_source_with_format(
                    Path::new("host.kdl"),
                    source.as_bytes(),
                    ConfigFormat::Kdl
                ),
                Err(ConfigSourceError::Parse {
                    format: "KDL 2",
                    ..
                })
            ),
            "accepted invalid native node: {source}"
        );
    }
}

#[test]
fn hocon_accepts_native_objects_and_arrays() {
    let directory = native_fixture_directory();
    let source = r#"
nginx_server = {
  path = "nginx.conf"
  root_prefix = "."
}
haproxy_server = [{
  paths = ["frontend.cfg", "backend.cfg"]
  node_ip = "10.0.0.11"
  gpu1_defined = false
}]
"#;

    let resolved = resolve_source(&directory.path().join("host.hocon"), source.as_bytes())
        .expect("resolved native HOCON");

    assert_eq!(resolved.config.listeners.len(), 2);
    assert_eq!(resolved.dependencies.len(), 4);
    assert!(resolved.compositional);
}

#[test]
fn uci_main_and_generic_json_compose_with_native_sections() {
    let directory = native_fixture_directory();
    let source = br"
config json 'root'
  option kind 'object'

config json 'listeners'
  option parent 'root'
  option key 'listeners'
  option kind 'array'

config oxiroute 'main'
  option version '1'

config nginx_server 'web'
  option path 'nginx.conf'
  option root_prefix '.'

config haproxy_server 'database'
  list path 'frontend.cfg'
  list path 'backend.cfg'
  option node_ip '10.0.0.11'
  option gpu1_defined '0'
";

    let resolved =
        resolve_source(&directory.path().join("host.uci"), source).expect("resolved friendly UCI");

    assert_eq!(resolved.config.version, 1);
    assert_eq!(resolved.config.listeners.len(), 2);
    assert_eq!(resolved.dependencies.len(), 4);
    assert!(resolved.compositional);
}

#[test]
fn lua_resolves_through_the_legacy_loader_and_matches_kdl() {
    let lua = resolve_source(
        Path::new("empty.lua"),
        b"return { version = 1, listeners = {} }",
    )
    .expect("resolved Lua");
    let kdl = resolve_source(Path::new("empty.kdl"), b"version 1\n(array)listeners {}\n")
        .expect("resolved KDL");

    assert_eq!(lua.config, kdl.config);
    assert_eq!(lua.canonical_kdl, kdl.canonical_kdl);
    assert!(!lua.compositional);
    assert!(lua.dependencies.is_empty());
}

#[test]
fn templates_expand_before_typing_and_preview_is_deterministic() {
    let source = br#"
templates = { empty = { listeners = [] } }
use = "empty"
version = 1
"#;
    let first = resolve_source(Path::new("templated.hocon"), source).expect("first resolution");
    let second = resolve_source(Path::new("templated.hocon"), source).expect("second resolution");

    assert!(first.compositional);
    assert_eq!(first, second);
    assert_eq!(
        decode_value(ConfigFormat::Kdl, first.canonical_kdl.as_bytes()).unwrap(),
        serde_json::to_value(&first.config).unwrap()
    );
}

#[test]
fn sanitized_phoenix_sources_resolve_as_one_host() {
    let fixture = live_fixture("phoenix");
    let source = br#"
nginx_server "nginx/nginx.conf" {
  root_prefix "nginx"
  host_timezone "America/Bahia"
  default_access_log_file "/var/lib/oxiroute/http-access.jsonl"
  recording_root "/mnt/cloud/4tb/cam-rtmp"
  default_error_server "nginx/1.30.2"
}
haproxy_server "haproxy.cfg" {
  node_ip "10.0.0.11"
  gpu1_defined #false
}
"#;

    let resolved = resolve_source(&fixture.join("host.kdl"), source).expect("Phoenix host");

    assert!(!resolved.config.http_services.is_empty());
    assert!(!resolved.config.rtmp_services.is_empty());
    assert!(!resolved.config.upstream_pools.is_empty());
    assert!(resolved.dependencies.len() >= 4);
}

#[test]
fn sanitized_back1_and_chicopc_haproxy_sources_use_explicit_environments() {
    for (host, node_ip) in [("back1", "10.0.0.7"), ("chicopc", "10.0.0.15")] {
        let fixture = live_fixture(host);
        let source = format!(
            "haproxy_server \"haproxy.cfg\" {{ node_ip \"{node_ip}\"; gpu1_defined #true; }}"
        );
        let resolved = resolve_source(&fixture.join("host.kdl"), source.as_bytes())
            .unwrap_or_else(|error| panic!("{host} did not resolve: {error}"));

        assert!(!resolved.config.listeners.is_empty());
        assert_eq!(
            resolved.dependencies,
            vec![fixture.join("haproxy.cfg"), fixture.clone()]
        );
    }
}

#[test]
fn newly_representable_native_candidate_resolves_with_complete_policy() {
    let fixture = fixture_root().join("haproxy/hostrouter-active.cfg");
    let path = serde_json::to_string(fixture.to_str().unwrap()).unwrap();
    let source = format!("haproxy_server = {{ paths = [{path}] }}");

    let resolved = resolve_source(Path::new("hostrouter.hocon"), source.as_bytes())
        .expect("representable HAProxy root");

    assert!(resolved.compositional);
    assert_eq!(
        resolved.dependencies,
        [fixture.clone(), fixture.parent().unwrap().to_path_buf()]
    );
    assert_eq!(resolved.config.listeners.len(), 1);
    assert_eq!(resolved.config.upstream_pools.len(), 1);
    assert_eq!(resolved.config.http_services[0].routes.len(), 2);
    let oxiroute_config::HttpRouteAction::Proxy { policy, .. } =
        &resolved.config.http_services[0].routes[0].action
    else {
        panic!("host route must proxy")
    };
    assert_eq!(policy.retry.max_retries, 3);
    assert!(policy.retry.final_redispatch);
}

#[test]
fn native_varnish_reference_resolves_exact_cache_subset_and_retains_evidence() {
    let root = fs::canonicalize(fixture_root().join("varnish/exact.vcl"))
        .expect("canonical Varnish fixture");
    let path = serde_json::to_string(root.to_str().expect("UTF-8 Varnish path")).unwrap();
    let source = format!(
        r#"varnish_server = {{
  path = {path}
  arguments = [
    "varnishd", "-a", ":6081", "-s", "cache=malloc,256M",
    "-p", "default_ttl=120s", "-p", "default_grace=10s",
    "-p", "default_keep=300s", "-F"
  ]
}}"#
    );

    let resolved = resolve_source(Path::new("varnish.hocon"), source.as_bytes())
        .expect("resolved exact Varnish HOCON");

    assert_eq!(resolved.config.listeners.len(), 1);
    assert_eq!(resolved.config.upstream_pools.len(), 1);
    assert_eq!(resolved.config.http_services.len(), 1);
    assert_eq!(resolved.config.cache_stores.len(), 1);
    assert_eq!(
        resolved.dependencies,
        [root.clone(), root.parent().unwrap().to_path_buf()]
    );
    let evidence = &resolved.native_references[0].evidence;
    assert_eq!(evidence.source.product, "varnish");
    assert_eq!(
        evidence.source.capability_profile.id,
        "varnish-vcl-exact-cache"
    );
    assert_eq!(evidence.source.version.as_deref(), Some("4.1"));
    assert!(evidence.candidate.finalized);
    assert!(
        evidence
            .candidate
            .provenance
            .iter()
            .any(|entry| entry.path == "/http_services/0/routes/0/action/policy/cache")
    );

    let kdl_source = format!(
        "varnish_server {path} \"varnishd\" \"-a\" \":6081\" \"-s\" \"cache=malloc,256M\" \"-p\" \"default_ttl=120s\" \"-p\" \"default_grace=10s\" \"-p\" \"default_keep=300s\" \"-F\"\n"
    );
    let kdl = resolve_source(Path::new("varnish.kdl"), kdl_source.as_bytes())
        .expect("resolved exact Varnish KDL");
    assert_eq!(kdl.config, resolved.config);

    let uci_source = format!(
        "config varnish_server 'cache'\n  option path {}\n  list arguments 'varnishd'\n  list arguments '-a'\n  list arguments ':6081'\n  list arguments '-s'\n  list arguments 'cache=malloc,256M'\n  list arguments '-p'\n  list arguments 'default_ttl=120s'\n  list arguments '-p'\n  list arguments 'default_grace=10s'\n  list arguments '-p'\n  list arguments 'default_keep=300s'\n  list arguments '-F'\n",
        root.display()
    );
    let uci = resolve_source(Path::new("varnish.uci"), uci_source.as_bytes())
        .expect("resolved exact Varnish UCI");
    assert_eq!(uci.config, resolved.config);
}

#[test]
fn native_varnish_reference_rejects_missing_invocation_semantics() {
    let root = fixture_root().join("varnish/exact.vcl");
    let path = serde_json::to_string(root.to_str().expect("UTF-8 Varnish path")).unwrap();
    let source = format!("varnish_server = {{ path = {path} }}");

    let error = resolve_source(Path::new("blocked-varnish.hocon"), source.as_bytes()).unwrap_err();
    assert!(matches!(error, ConfigSourceError::NativeImport { .. }));
    assert!(error.to_string().contains("E_VCL_SEMANTIC_MISMATCH"));
}

#[test]
fn inline_and_native_identity_collisions_are_rejected_by_composition() {
    let fixture = fixture_root().join("haproxy/minimal-representable.cfg");
    let path = serde_json::to_string(fixture.to_str().unwrap()).unwrap();
    let source = format!(
        r#"
version = 1
listeners = []
upstream_pools = [{{
  name = "postgres_pool"
  servers = [{{
    name = "inline"
    endpoint = {{ type = "socket", address = "127.0.0.1:5433" }}
  }}]
}}]
haproxy_server = {{ paths = [{path}] }}
"#
    );

    assert!(matches!(
        resolve_source(Path::new("collision.hocon"), source.as_bytes()),
        Err(ConfigSourceError::Composition(_))
    ));
}

fn native_fixture_directory() -> tempfile::TempDir {
    let directory = tempdir().expect("temporary native sources");
    fs::write(
        directory.path().join("nginx.conf"),
        b"events { worker_connections 16; } http { access_log off; server { listen 127.0.0.1:18080 default_server; location / { return 200 ok; } } }",
    )
    .expect("nginx fixture");
    fs::write(
        directory.path().join("frontend.cfg"),
        b"defaults tcp_defaults\n  mode tcp\n  retries 0\n  timeout connect 10s\n  timeout queue 15s\n  timeout client 5m\n  timeout server 5m\nfrontend database\n  bind 127.0.0.1:15432\n  default_backend database_pool\n",
    )
    .expect("HAProxy frontend fixture");
    fs::write(
        directory.path().join("backend.cfg"),
        b"backend database_pool\n  balance roundrobin\n  server primary 127.0.0.1:5432\n",
    )
    .expect("HAProxy backend fixture");
    directory
}

fn fixture_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../oxiroute-import/tests/fixtures")
}

fn live_fixture(host: &str) -> std::path::PathBuf {
    fixture_root().join("live").join(host)
}
