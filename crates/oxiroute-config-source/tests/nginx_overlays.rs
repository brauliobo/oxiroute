#![cfg(unix)]

use std::fs;

use oxiroute_config::{UpstreamTls, UpstreamTlsVerify};
use oxiroute_config_source::{
    ConfigFormat, ConfigSourceError, resolve_source, resolve_source_with_format,
};
use tempfile::{TempDir, tempdir};

const NGINX: &str = "events {} http { access_log off; proxy_http_version 1.1; proxy_buffering off; \
proxy_request_buffering off; \
proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset; \
server { listen 127.0.0.1:18080 default_server; server_name api.example.org; \
location / { if ($http_authorization != \"Bearer <redacted>\") { return 401; } \
proxy_pass https://192.0.2.44; } } }";

const KDL: &str = r#"
nginx_server "nginx.conf" {
  root_prefix "."
  bearer_token_file "api.example.org" "/run/secrets/api.token"
  upstream_tls "192.0.2.44" "origin.example.test" "/path/to/ca.pem"
}
"#;

const HOCON: &str = r#"
nginx_server = {
  path = "nginx.conf"
  root_prefix = "."
  bearer_token_file = [{ server_name = "api.example.org", path = "/run/secrets/api.token" }]
  upstream_tls = [{ authority = "192.0.2.44", server_name = "origin.example.test", ca_certificate_path = "/path/to/ca.pem" }]
}
"#;

const UCI: &str = "
config nginx_server 'web'
  option path 'nginx.conf'
  option root_prefix '.'
  list bearer_token_file 'api.example.org=/run/secrets/api.token'
  list upstream_tls '192.0.2.44=origin.example.test,ca=/path/to/ca.pem'
";

fn fixture() -> TempDir {
    let directory = tempdir().expect("nginx overlay fixture");
    fs::write(directory.path().join("nginx.conf"), NGINX).expect("nginx root");
    directory
}

fn resolve(
    directory: &TempDir,
    name: &str,
    source: &str,
) -> Result<oxiroute_config_source::ResolvedSource, ConfigSourceError> {
    resolve_source(&directory.path().join(name), source.as_bytes())
}

fn assert_overlays_applied(source: &oxiroute_config_source::ResolvedSource) {
    let draft = source.config.as_draft();
    assert_eq!(
        draft.upstream_pools[0].tls,
        Some(UpstreamTls {
            server_name: Some("origin.example.test".into()),
            verify: UpstreamTlsVerify::Full,
            ca_certificate_path: Some("/path/to/ca.pem".into()),
        })
    );
    let policies = format!("{:?}", draft.http_services);
    assert!(policies.contains("BearerTokenFile"), "{policies}");
    assert!(policies.contains("/run/secrets/api.token"), "{policies}");
}

#[test]
fn kdl_hocon_and_uci_apply_the_same_overlays() {
    let directory = fixture();
    let kdl = resolve(&directory, "host.kdl", KDL).expect("KDL overlays");
    assert_overlays_applied(&kdl);
    for (name, source) in [("host.hocon", HOCON), ("host.uci", UCI)] {
        let other =
            resolve(&directory, name, source).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(other.config, kdl.config, "{name}");
    }
}

#[test]
fn overlays_are_optional_ca_and_repeatable() {
    let directory = fixture();
    let source = r#"
nginx_server "nginx.conf" {
  root_prefix "."
  bearer_token_file "api.example.org" "/run/secrets/api.token"
  upstream_tls "192.0.2.44" "origin.example.test"
}
"#;
    let resolved = resolve(&directory, "host.kdl", source).expect("overlays without a CA");
    assert_eq!(
        resolved.config.as_draft().upstream_pools[0]
            .tls
            .as_ref()
            .unwrap()
            .ca_certificate_path,
        None
    );
}

#[test]
fn overlays_compose_with_live_kdl() {
    let directory = fixture();
    let source = format!("version 1\n(object)management {{ bind \"127.0.0.1:18081\" }}\n{KDL}");
    let resolved = resolve(&directory, "live.kdl", &source).expect("live KDL with overlays");
    assert!(resolved.compositional);
    assert!(resolved.config.as_draft().management.is_some());
    assert_overlays_applied(&resolved);
}

#[test]
fn unmatched_overlays_fail_the_import() {
    let directory = fixture();
    for option in [
        r#"bearer_token_file "other.example.org" "/run/secrets/api.token""#,
        r#"upstream_tls "192.0.2.99" "origin.example.test""#,
    ] {
        let source = format!(
            "nginx_server \"nginx.conf\" {{ root_prefix \".\"; bearer_token_file \"api.example.org\" \"/run/secrets/a\"; \
             upstream_tls \"192.0.2.44\" \"o.example.test\"; {option} }}"
        );
        assert!(
            matches!(
                resolve(&directory, "host.kdl", &source),
                Err(ConfigSourceError::NativeImport { .. })
            ),
            "{option}"
        );
    }
}

#[test]
fn invalid_overlay_values_are_rejected_in_every_format() {
    let directory = fixture();
    let cases = [
        (ConfigFormat::Kdl, "bearer_token_file \"a.example.org\""),
        (
            ConfigFormat::Kdl,
            "bearer_token_file \"a.example.org\" \"/p\" \"extra\"",
        ),
        (ConfigFormat::Kdl, "bearer_token_file \"\" \"/p\""),
        (ConfigFormat::Kdl, "bearer_token_file a.example.org=\"/p\""),
        (ConfigFormat::Kdl, "bearer_token_file 1 2"),
        (
            ConfigFormat::Kdl,
            "bearer_token_file \"a.example.org\" \"/p\" { nested \"x\" }",
        ),
        (ConfigFormat::Kdl, "upstream_tls \"192.0.2.44\""),
        (
            ConfigFormat::Kdl,
            "upstream_tls \"192.0.2.44\" \"s\" \"/ca\" \"extra\"",
        ),
        (ConfigFormat::Kdl, "upstream_tls \"192.0.2.44\" \"\""),
        (
            ConfigFormat::Kdl,
            "bearer_token_file \"a\" \"/p\"; bearer_token_file \"A\" \"/q\"",
        ),
        (
            ConfigFormat::Kdl,
            "upstream_tls \"192.0.2.44\" \"s\"; upstream_tls \"192.0.2.44\" \"t\"",
        ),
    ];
    for (format, option) in cases {
        let source = format!("nginx_server \"nginx.conf\" {{ {option} }}");
        let result = resolve_source_with_format(
            &directory.path().join("host.kdl"),
            source.as_bytes(),
            format,
        );
        assert!(
            matches!(
                result,
                Err(ConfigSourceError::Parse {
                    format: "KDL 2",
                    ..
                })
            ),
            "accepted invalid option: {option}: {result:?}"
        );
    }
    let hocon = [
        "bearer_token_file = [{ server_name = \"a\" }]",
        "bearer_token_file = [{ server_name = \"a\", path = \"/p\", extra = 1 }]",
        "bearer_token_file = \"a=/p\"",
        "upstream_tls = [{ authority = \"192.0.2.44\" }]",
        "upstream_tls = [{ authority = \"a\", server_name = \"s\" }, { authority = \"A\", server_name = \"t\" }]",
    ];
    for option in hocon {
        let source = format!("nginx_server = {{ path = \"nginx.conf\"\n{option} }}");
        assert!(
            resolve(&directory, "host.hocon", &source).is_err(),
            "accepted {option}"
        );
    }
    let uci = [
        "list bearer_token_file 'a.example.org'",
        "list bearer_token_file '=/p'",
        "list upstream_tls '192.0.2.44'",
        "list upstream_tls '192.0.2.44=s,ca'",
        "list upstream_tls '192.0.2.44=s,activate'",
        "option bearer_token_file 'a=/p'",
        "list bearer_token_file 'a=/p'\n  list bearer_token_file 'A=/q'",
    ];
    for entry in uci {
        let source = format!("config nginx_server 'web'\n  option path 'nginx.conf'\n  {entry}\n");
        assert!(
            matches!(
                resolve(&directory, "host.uci", &source),
                Err(ConfigSourceError::Parse { format: "UCI", .. })
            ),
            "accepted {entry}"
        );
    }
}
