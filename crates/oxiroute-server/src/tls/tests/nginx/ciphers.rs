#[allow(clippy::wildcard_imports)]
use super::*;

const RUSTLS_LIST: &str = "ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384";
const DHE_LIST: &str = "ECDHE-RSA-AES128-GCM-SHA256:DHE-RSA-AES256-GCM-SHA384";
const PARTLY_SERVED_LIST: &str = "ECDHE-RSA-AES128-GCM-SHA256:AES128-SHA";
const BOTH_VERSIONS: &str = "TLSv1.2 TLSv1.3";

fn imported_backend(directives: &str, protocols: &str) -> TlsBackend {
    let temp = TempDir::new().unwrap();
    let files = write_identity(temp.path(), "primary", "www.example.test", false);
    let dhparam = temp.path().join("dhparam.pem");
    fs::write(
        &dhparam,
        Dh::get_2048_256().unwrap().params_to_pem().unwrap(),
    )
    .unwrap();
    let source = format!(
        "http {{ access_log off; ssl_protocols {protocols}; {directives}
          server {{
            listen 127.0.0.1:8443 ssl default_server;
            server_name www.example.test;
            ssl_certificate {};
            ssl_certificate_key {};
            location / {{ return 204; }}
          }}
        }}",
        files.chain.display(),
        files.key.display(),
    )
    .replace("@DHPARAM@", &dhparam.display().to_string());
    fs::write(temp.path().join("nginx.conf"), source).unwrap();

    let report = oxiroute_import::nginx::import_http_fragment(Path::new("nginx.conf"), temp.path());
    assert!(
        report.blocked_services.is_empty(),
        "{:?}",
        report.diagnostics
    );
    let prepared = prepare_tls(report.validated().expect("imported nginx config")).unwrap();
    let profile = prepared.profiles().values().next().expect("TLS profile");
    match profile.backend() {
        TlsBackend::Rustls => assert!(profile.rustls_settings(0).is_ok(), "{directives}"),
        TlsBackend::Openssl => assert!(profile.tls_settings(0).is_ok(), "{directives}"),
    }
    profile.backend()
}

fn backend_of(directives: &str) -> TlsBackend {
    imported_backend(directives, BOTH_VERSIONS)
}

#[test]
fn an_imported_cipher_macro_runs_on_rustls() {
    for directives in [
        "ssl_ciphers HIGH:!aNULL:!MD5;",
        "ssl_ciphers HIGH:!aNULL:!MD5; ssl_prefer_server_ciphers off;",
        "ssl_ciphers ECDHE-RSA-AES128-GCM-SHA256:HIGH:!aNULL;",
        "ssl_ciphers ECDHE+AESGCM:!RC4; ssl_prefer_server_ciphers on;",
    ] {
        assert_eq!(backend_of(directives), TlsBackend::Rustls, "{directives}");
    }
}

#[test]
fn an_imported_explicit_list_rustls_serves_runs_on_rustls() {
    for list in [RUSTLS_LIST, PARTLY_SERVED_LIST] {
        let directives = format!("ssl_ciphers {list};");

        assert_eq!(backend_of(&directives), TlsBackend::Rustls, "{list}");
    }
}

#[test]
fn an_imported_explicit_dhe_list_selects_openssl() {
    for list in [
        DHE_LIST,
        "HIGH:DHE-RSA-AES128-GCM-SHA256",
        "AES128-SHA:AES256-SHA",
    ] {
        let directives = format!("ssl_ciphers {list};");

        assert_eq!(backend_of(&directives), TlsBackend::Openssl, "{list}");
    }
}

#[test]
fn an_imported_ssl_dhparam_selects_openssl_with_or_without_a_cipher_macro() {
    for directives in [
        "ssl_dhparam @DHPARAM@;",
        "ssl_ciphers HIGH:!aNULL:!MD5; ssl_dhparam @DHPARAM@;",
    ] {
        assert_eq!(backend_of(directives), TlsBackend::Openssl, "{directives}");
    }
}

#[test]
fn an_imported_tls13_minimum_leaves_the_cipher_list_out_of_the_backend_choice() {
    for directives in [
        "ssl_ciphers HIGH:!aNULL:!MD5;",
        "ssl_ciphers HIGH:!aNULL:!MD5; ssl_prefer_server_ciphers off;",
    ] {
        assert_eq!(
            imported_backend(directives, "TLSv1.3"),
            TlsBackend::Rustls,
            "{directives}"
        );
    }
}
