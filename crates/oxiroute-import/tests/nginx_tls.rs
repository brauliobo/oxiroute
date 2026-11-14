#![cfg(unix)]

use std::{fs, path::Path};

use openssl::{
    asn1::Asn1Time,
    bn::BigNum,
    ec::{EcGroup, EcKey},
    hash::MessageDigest,
    nid::Nid,
    pkey::PKey,
    x509::{X509, X509NameBuilder, extension::SubjectAlternativeName},
};
use oxiroute_config::{AlpnProtocol, CertificateSource, ConfigDraft};
use oxiroute_import::{
    E_DUPLICATE_IDENTITY, E_SEMANTICS_NOT_REPRESENTABLE, E_SOURCE_IO, Severity,
    nginx::{ImportReport, import_http_fragment},
};
use tempfile::TempDir;

const BOTH_ALPN: [AlpnProtocol; 2] = [AlpnProtocol::H2, AlpnProtocol::Http11];

fn write_lineage(live: &Path, lineage: &str, sans: &[&str]) {
    let key = PKey::from_ec_key(
        EcKey::generate(&EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap()).unwrap(),
    )
    .unwrap();
    let mut name = X509NameBuilder::new().unwrap();
    name.append_entry_by_text("CN", lineage).unwrap();
    let name = name.build();
    let mut builder = X509::builder().unwrap();
    builder.set_version(2).unwrap();
    let serial = BigNum::from_u32(1).unwrap().to_asn1_integer().unwrap();
    builder.set_serial_number(&serial).unwrap();
    builder.set_subject_name(&name).unwrap();
    builder.set_issuer_name(&name).unwrap();
    builder.set_pubkey(&key).unwrap();
    builder
        .set_not_before(&Asn1Time::days_from_now(0).unwrap())
        .unwrap();
    builder
        .set_not_after(&Asn1Time::days_from_now(30).unwrap())
        .unwrap();
    let mut alternative_names = SubjectAlternativeName::new();
    for san in sans {
        alternative_names.dns(san);
    }
    let extension = alternative_names
        .build(&builder.x509v3_context(None, None))
        .unwrap();
    builder.append_extension(extension).unwrap();
    builder.sign(&key, MessageDigest::sha256()).unwrap();
    let directory = live.join(lineage);
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("cert.pem"),
        builder.build().to_pem().unwrap(),
    )
    .unwrap();
}

struct Tree {
    directory: TempDir,
}

impl Tree {
    fn new(lineages: &[(&str, &[&str])]) -> Self {
        let directory = tempfile::tempdir().unwrap();
        for (lineage, sans) in lineages {
            write_lineage(&directory.path().join("live"), lineage, sans);
        }
        Self { directory }
    }

    fn import(&self, source: &str) -> ImportReport {
        let live = self.directory.path().join("live");
        let source = source.replace("@LIVE@", &live.to_string_lossy());
        fs::write(self.directory.path().join("nginx.conf"), source).unwrap();
        import_http_fragment(Path::new("nginx.conf"), self.directory.path())
    }

    fn import_fixture(&self, name: &str) -> ImportReport {
        let path = Path::new("tests/fixtures/nginx/synthetic/edge").join(name);
        self.import(&fs::read_to_string(path).unwrap())
    }
}

fn draft(report: &ImportReport) -> &ConfigDraft {
    assert!(
        report.blocked_services.is_empty(),
        "{:?}",
        report.diagnostics
    );
    report.validated().map_or_else(
        || panic!("not finalized: {:?}", report.diagnostics),
        oxiroute_config::ValidatedConfig::as_draft,
    )
}

fn codes(report: &ImportReport, severity: Severity) -> Vec<&'static str> {
    report
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity() == severity)
        .map(|diagnostic| diagnostic.code().as_str())
        .collect()
}

fn server(listen: &str, name: &str) -> String {
    format!(
        "server {{ listen {listen}; server_name {name}; ssl_certificate @LIVE@/example.com/fullchain.pem; \
         ssl_certificate_key @LIVE@/example.com/privkey.pem; location / {{ return 204; }} }}\n"
    )
}

fn http(servers: &[String]) -> String {
    format!(
        "http {{ access_log off; ssl_protocols TLSv1.2 TLSv1.3;\n{} }}",
        servers.concat()
    )
}

fn example_com_tree() -> Tree {
    Tree::new(&[("example.com", &["example.com", "*.example.com"])])
}

#[test]
fn mixed_http2_flags_on_one_socket_enable_h2_for_all_and_warn_about_the_rest() {
    let report = example_com_tree().import_fixture("http2-mixed/nginx.conf");
    let config = draft(&report);

    assert_eq!(config.tls_profiles.len(), 1);
    assert_eq!(config.tls_profiles[0].alpn, BOTH_ALPN);
    assert_eq!(codes(&report, Severity::Warning), ["W_HTTP2_SOCKET_WIDE"]);
    assert!(!report.has_errors());
}

#[test]
fn unanimous_http2_directive_values_choose_the_alpn_without_warnings() {
    let tree = example_com_tree();
    for (directive, alpn) in [
        ("http2 on;", &BOTH_ALPN[..]),
        ("http2 off;", &[AlpnProtocol::Http11][..]),
        ("", &[AlpnProtocol::Http11][..]),
    ] {
        let servers =
            ["a", "b"].map(|name| server("192.0.2.10:443 ssl", &format!("{name}.example.com")));
        let source = http(&servers).replacen("http {", &format!("http {{ {directive}"), 1);
        let report = tree.import(&source);

        assert_eq!(draft(&report).tls_profiles[0].alpn, alpn, "{directive}");
        assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    }
}

#[test]
fn http2_listen_flag_wins_over_an_explicit_off_directive() {
    let servers = [
        server("192.0.2.10:443 ssl http2", "a.example.com"),
        server("192.0.2.10:443 ssl", "b.example.com").replace("{ listen", "{ http2 off; listen"),
    ];
    let report = example_com_tree().import(&http(&servers));

    assert_eq!(draft(&report).tls_profiles[0].alpn, BOTH_ALPN);
    assert_eq!(codes(&report, Severity::Warning), ["W_HTTP2_SOCKET_WIDE"]);
}

#[test]
fn mixed_http2_directives_without_a_listen_flag_stay_blocked() {
    let servers = [
        server("192.0.2.10:443 ssl", "a.example.com").replace("{ listen", "{ http2 on; listen"),
        server("192.0.2.10:443 ssl", "b.example.com"),
    ];
    let report = example_com_tree().import(&http(&servers));

    assert_eq!(report.blocked_services.len(), 1);
    assert_eq!(
        codes(&report, Severity::Error),
        [E_SEMANTICS_NOT_REPRESENTABLE.as_str()]
    );
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message()
            .contains("mismatched HTTP/2 policies and no listen flag")
    }));
}

#[test]
fn plaintext_bind_with_effective_http2_stays_blocked() {
    for listen in ["192.0.2.10:80 http2", "192.0.2.10:80"] {
        let source = format!(
            "http {{ access_log off; {}server {{ listen {listen}; server_name a.example.com; location / {{ return 204; }} }} }}",
            if listen.ends_with("http2") {
                ""
            } else {
                "http2 on; "
            }
        );
        let report = Tree::new(&[]).import(&source);

        assert_eq!(report.blocked_services.len(), 1, "{listen}");
        assert!(report.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message()
                .contains("plaintext nginx HTTP/2 cannot be represented")
        }));
    }
}

#[test]
fn certbot_lineage_dns_names_are_the_leaf_sans_and_uncovered_names_warn() {
    let tree = Tree::new(&[
        ("example.com", &["example.com", "*.example.com"]),
        ("example.org", &["example.org"]),
    ]);
    let report = tree.import_fixture("certbot-lineages/nginx.conf");
    let config = draft(&report);

    let names = config
        .certificates
        .iter()
        .map(|certificate| certificate.dns_names.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [vec!["*.example.com", "example.com"], vec!["example.org"]]
    );
    assert!(matches!(
        config.certificates[0].source,
        CertificateSource::Certbot { .. }
    ));
    assert_eq!(
        codes(&report, Severity::Warning),
        ["W_CERTIFICATE_NAME_UNCOVERED"]
    );
    let warning = report
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.severity() == Severity::Warning)
        .unwrap();
    assert!(warning.message().contains("c.example.net"));
    assert!(!report.has_errors());
}

#[test]
fn covered_names_produce_no_certificate_warnings() {
    let tree = example_com_tree();
    let servers = [
        "a.example.com",
        "B.Example.COM",
        "example.com",
        "*.example.com",
    ]
    .map(|name| server("192.0.2.10:443 ssl", name));
    let leading_dot = [server("192.0.2.10:443 ssl", ".example.com")];
    for servers in [&servers[..], &leading_dot[..]] {
        let report = tree.import(&http(servers));

        draft(&report);
        assert_eq!(codes(&report, Severity::Warning), Vec::<&str>::new());
    }
}

#[test]
fn two_lineages_claiming_one_san_in_one_profile_are_an_error() {
    let tree = Tree::new(&[
        ("example.com", &["example.com", "*.example.com"]),
        ("example.org", &["example.org", "example.com"]),
    ]);
    let source = http(&[
        server("192.0.2.10:443 ssl", "a.example.com"),
        server("192.0.2.10:443 ssl", "example.org").replace("example.com/", "example.org/"),
    ]);
    let report = tree.import(&source);

    assert_eq!(report.blocked_services.len(), 1);
    assert_eq!(
        codes(&report, Severity::Error),
        [E_DUPLICATE_IDENTITY.as_str()]
    );
}

#[test]
fn lineages_with_overlapping_sans_on_separate_binds_do_not_conflict() {
    let tree = Tree::new(&[
        ("example.com", &["example.com", "*.example.com"]),
        ("example.org", &["example.org", "example.com"]),
    ]);
    let source = http(&[
        server("192.0.2.10:443 ssl", "a.example.com"),
        server("192.0.2.11:443 ssl", "example.org").replace("example.com/", "example.org/"),
    ]);

    assert_eq!(draft(&tree.import(&source)).certificates.len(), 2);
}

#[test]
fn unreadable_lineage_certificate_is_missing_identity_evidence() {
    let tree = Tree::new(&[]);
    let report = tree.import(&http(&[server("192.0.2.10:443 ssl", "a.example.com")]));

    assert_eq!(report.blocked_services.len(), 1);
    assert_eq!(codes(&report, Severity::Error), [E_SOURCE_IO.as_str()]);
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message()
            .contains("certificate identity evidence unavailable")
    }));
}

#[test]
fn plain_file_certificates_keep_server_name_identities() {
    let source = "http { access_log off; ssl_protocols TLSv1.2 TLSv1.3; server { listen 192.0.2.10:443 ssl; \
        server_name a.example.com; ssl_certificate /path/to/chain.pem; ssl_certificate_key /path/to/key.pem; \
        location / { return 204; } } }";
    let report = Tree::new(&[]).import(source);

    assert_eq!(draft(&report).certificates[0].dns_names, ["a.example.com"]);
}

#[test]
fn dual_stack_pair_merges_listen_flags_of_both_sockets() {
    let tree = example_com_tree();
    let source = http(&[server(
        "443 ssl; listen [::]:443 ssl http2",
        "a.example.com",
    )]);
    let report = tree.import(&source);
    let config = draft(&report);

    assert_eq!(config.listeners.len(), 1);
    assert_eq!(config.tls_profiles[0].alpn, BOTH_ALPN);

    let disagreeing = http(&[server("443 ssl; listen [::]:443", "a.example.com")]);
    let report = tree.import(&disagreeing);
    assert!(report.validated().is_none());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message().contains("disagree about ssl"))
    );
}
