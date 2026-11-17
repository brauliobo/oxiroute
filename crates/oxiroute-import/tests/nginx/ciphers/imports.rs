use oxiroute_config::TlsVersion;

use crate::support::{
    DHPARAM, import, import_with_protocols, policy, profile, warning_codes, warnings,
};

const MACROS: [&str; 9] = [
    "HIGH:!aNULL:!MD5",
    "HIGH",
    "MEDIUM",
    "LOW",
    "DEFAULT",
    "ALL:!aNULL:!eNULL",
    "HIGH:!aNULL:!MD5:@STRENGTH",
    "ECDHE+AESGCM:!RC4",
    "-ALL:+HIGH",
];
const RUSTLS_LIST: &str = "ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384:ECDHE-ECDSA-CHACHA20-POLY1305:ECDHE-RSA-CHACHA20-POLY1305";
const DHE_LIST: &str = "ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:DHE-RSA-AES128-GCM-SHA256:DHE-RSA-AES256-GCM-SHA384";
const CBC_LIST: &str = "AES128-SHA:AES256-SHA";
const PARTLY_SERVED_LIST: &str = "ECDHE-RSA-AES128-GCM-SHA256:AES128-SHA";

fn ciphers(value: &str) -> String {
    format!("ssl_ciphers \"{value}\";")
}

#[test]
fn a_cipher_macro_imports_as_the_default_policy_with_one_explanatory_note() {
    for value in MACROS {
        let report = import(&ciphers(value));
        let profile = profile(&report);

        assert_eq!(profile.policy.cipher_list, None, "{value}");
        assert_eq!(profile.policy.backend, None, "{value}");
        assert_eq!(
            warning_codes(&report),
            ["W_TLS_CIPHERS_DEFAULTED"],
            "{value}"
        );
    }
}

#[test]
fn the_macro_note_names_the_default_list_the_dropped_suites_and_the_legacy_client_recipe() {
    let report = import(&ciphers("HIGH:!aNULL:!MD5"));

    let message = warnings(&report)[0].message();
    for expected in [
        "`HIGH:!aNULL:!MD5`",
        "OpenSSL cipher macro",
        "default cipher policy",
        "TLS 1.3 AES-GCM and ChaCha20-Poly1305",
        "TLS 1.2 ECDHE AEAD suites",
        "DHE-RSA, CBC and RSA key-exchange suites are not offered",
        "separate listener and TLS profile",
        "backend = \"openssl\"",
        "explicit `cipher_list`",
        "Serving legacy clients",
        "docs/OPERATIONS.md",
    ] {
        assert!(message.contains(expected), "{expected}: {message}");
    }
}

#[test]
fn an_explicit_list_rustls_supports_imports_unchanged_without_a_note() {
    let report = import(&ciphers(RUSTLS_LIST));

    assert_eq!(policy(&report).cipher_list.as_deref(), Some(RUSTLS_LIST));
    assert_eq!(policy(&report).backend, None);
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
}

#[test]
fn tls13_suite_names_in_an_explicit_list_are_served_by_rustls_without_a_note() {
    let list = format!("TLS_AES_128_GCM_SHA256:{RUSTLS_LIST}");
    let report = import(&ciphers(&list));

    assert_eq!(policy(&report).cipher_list.as_deref(), Some(list.as_str()));
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
}

#[test]
fn an_explicit_list_with_dhe_imports_unchanged_and_the_note_says_openssl_serves_it() {
    let report = import(&ciphers(DHE_LIST));

    assert_eq!(policy(&report).cipher_list.as_deref(), Some(DHE_LIST));
    assert_eq!(policy(&report).backend, None);
    assert_eq!(warning_codes(&report), ["W_TLS_BACKEND_OPENSSL"]);
    let message = warnings(&report)[0].message();
    assert!(message.contains("DHE-RSA suites"), "{message}");
    assert!(message.contains("imported unchanged"), "{message}");
    assert!(message.contains("OpenSSL backend"), "{message}");
}

#[test]
fn an_explicit_list_without_a_tls12_suite_rustls_serves_selects_openssl_through_the_note() {
    let report = import(&ciphers(CBC_LIST));

    assert_eq!(policy(&report).cipher_list.as_deref(), Some(CBC_LIST));
    assert_eq!(warning_codes(&report), ["W_TLS_BACKEND_OPENSSL"]);
    assert!(warnings(&report)[0].message().contains("no TLS 1.2 suite"));
}

#[test]
fn an_explicit_list_that_rustls_serves_only_in_part_stays_on_rustls_and_names_what_it_lacks() {
    let report = import(&ciphers(PARTLY_SERVED_LIST));

    assert_eq!(
        policy(&report).cipher_list.as_deref(),
        Some(PARTLY_SERVED_LIST)
    );
    assert_eq!(warning_codes(&report), ["W_TLS_CIPHERS_LEFT_OUT"]);
    let message = warnings(&report)[0].message();
    assert!(message.contains("(AES128-SHA)"), "{message}");
    assert!(message.contains("Serving legacy clients"), "{message}");
}

#[test]
fn a_list_mixing_explicit_suites_and_a_macro_imports_as_the_default_policy() {
    for value in [
        "ECDHE-RSA-AES128-GCM-SHA256:HIGH:!aNULL",
        "ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384:!aNULL:!MD5",
    ] {
        let report = import(&ciphers(value));

        assert_eq!(policy(&report).cipher_list, None, "{value}");
        assert_eq!(
            warning_codes(&report),
            ["W_TLS_CIPHERS_DEFAULTED"],
            "{value}"
        );
    }
}

#[test]
fn a_macro_list_that_names_a_dhe_suite_imports_unchanged_for_openssl() {
    let value = "ECDHE-RSA-AES128-GCM-SHA256:DHE-RSA-AES128-GCM-SHA256:HIGH:!aNULL";
    let report = import(&ciphers(value));

    assert_eq!(policy(&report).cipher_list.as_deref(), Some(value));
    assert_eq!(warning_codes(&report), ["W_TLS_BACKEND_OPENSSL"]);
}

#[test]
fn ssl_dhparam_maps_to_the_dh_parameters_path_and_notes_the_openssl_backend() {
    let report = import(&format!("ssl_dhparam {DHPARAM};"));

    assert_eq!(policy(&report).dh_parameters_path, Some(DHPARAM.into()));
    assert_eq!(policy(&report).cipher_list, None);
    assert_eq!(warning_codes(&report), ["W_TLS_BACKEND_OPENSSL"]);
    assert!(
        warnings(&report)[0]
            .message()
            .contains("dh_parameters_path")
    );
}

#[test]
fn a_macro_with_ssl_dhparam_keeps_its_openssl_meaning_on_the_openssl_profile() {
    let report = import(&format!(
        "ssl_ciphers HIGH:!aNULL:!MD5; ssl_dhparam {DHPARAM};"
    ));

    assert_eq!(
        policy(&report).cipher_list.as_deref(),
        Some("HIGH:!aNULL:!MD5")
    );
    assert_eq!(policy(&report).dh_parameters_path, Some(DHPARAM.into()));
    assert_eq!(warning_codes(&report), ["W_TLS_BACKEND_OPENSSL"]);
}

#[test]
fn a_tls13_only_minimum_imports_a_macro_without_a_note_because_the_list_governs_tls12_only() {
    let report = import_with_protocols(&ciphers("HIGH:!aNULL:!MD5"), "TLSv1.3");
    let profile = profile(&report);

    assert_eq!(profile.min_version, TlsVersion::Tls13);
    assert_eq!(profile.policy.cipher_list, None);
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
}

#[test]
fn the_tls_protocol_policy_still_sets_the_minimum_version() {
    for (protocols, minimum) in [
        ("TLSv1.2 TLSv1.3", TlsVersion::Tls12),
        ("TLSv1.3", TlsVersion::Tls13),
    ] {
        let report = import_with_protocols(&ciphers("HIGH:!aNULL:!MD5"), protocols);

        assert_eq!(profile(&report).min_version, minimum, "{protocols}");
    }
}

#[test]
fn ssl_prefer_server_ciphers_maps_independently_of_the_cipher_import() {
    for (directive, expected) in [
        ("ssl_prefer_server_ciphers on;", true),
        ("ssl_prefer_server_ciphers off;", false),
        ("", true),
    ] {
        let report = import(&format!("ssl_ciphers HIGH:!aNULL:!MD5; {directive}"));

        assert_eq!(
            policy(&report).prefer_server_ciphers,
            expected,
            "{directive}"
        );
        assert_eq!(policy(&report).cipher_list, None, "{directive}");
    }
}
