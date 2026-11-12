#![cfg(unix)]
#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! What `curl` and `openssl s_client` see of a TLS listener, on the OpenSSL and the rustls backend alike: protocol
//! versions, ALPN, suites, resumption and client certificates. Both tools must be installed.

mod support;

use std::{
    io::Write as _,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

use oxiroute_config::{
    AlpnProtocol, HttpVersionPolicy, TlsBackend, TlsClientAuthMode, TlsClientAuthPolicy, TlsPolicy,
    TlsSessionCache, TlsVersion,
};
use tokio::{task::spawn_blocking, time::timeout};

use support::{
    PROXY_SERVER_NAME, PlainH1Origin, ProxyHarness, ReservedListener, fixture,
    generate_test_only_client_chain, proxy_config,
};

const LONG_TIMEOUT: Duration = Duration::from_mins(2);
const BACKENDS: [TlsBackend; 2] = [TlsBackend::Openssl, TlsBackend::Rustls];
const REQUEST: &[u8] =
    b"GET /client HTTP/1.1\r\nHost: proxy.example.test\r\nConnection: close\r\n\r\n";

async fn with_listener<T>(
    backend: TlsBackend,
    alpn: Vec<AlpnProtocol>,
    configure: impl FnOnce(&mut oxiroute_config::TlsProfile),
    test: impl AsyncFnOnce(u16) -> T,
) -> T {
    let origin = PlainH1Origin::start(b"clients").await;
    let reserved = ReservedListener::new();
    let mut config = proxy_config(
        reserved.address,
        origin.address,
        alpn,
        None,
        HttpVersionPolicy::default(),
    );
    config.tls_profiles[0].policy.backend = backend;
    configure(&mut config.tls_profiles[0]);
    let proxy = ProxyHarness::start(&config.validate(), reserved);
    let result = test(proxy.address.port()).await;
    proxy.finish().await;
    origin.finish().await;
    result
}

/// Runs `command` to its end with `input` on its standard input; returns its success and its output (both streams).
async fn run(mut command: Command, input: &'static [u8]) -> (bool, String) {
    spawn_blocking(move || {
        let program = command.get_program().to_owned();
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| {
                panic!("{} is required by this test: {error}", program.display())
            });
        // The tool may stop reading before the input is written; that is not what the test looks at.
        let _ = child.stdin.take().expect("piped stdin").write_all(input);
        let output = child.wait_with_output().expect("tool output");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (output.status.success(), text)
    })
    .await
    .expect("tool task")
}

fn curl(port: u16, flags: &[&str]) -> Command {
    let mut command = Command::new("curl");
    command
        .args(["--silent", "--show-error", "--max-time", "15", "--cacert"])
        .arg(fixture("ca-a.pem"))
        .arg("--resolve")
        .arg(format!("{PROXY_SERVER_NAME}:{port}:127.0.0.1"))
        .args(["--write-out", "\n[%{http_code} http%{http_version}]"])
        .args(flags)
        .arg(format!("https://{PROXY_SERVER_NAME}:{port}/curl"));
    command
}

fn s_client(port: u16, flags: &[&str]) -> Command {
    let mut command = Command::new("timeout");
    command
        .args(["30", "openssl", "s_client", "-ign_eof", "-connect"])
        .arg(format!("127.0.0.1:{port}"))
        .args(["-servername", PROXY_SERVER_NAME, "-CAfile"])
        .arg(fixture("ca-a.pem"))
        .args(flags);
    command
}

#[tokio::test]
async fn curl_speaks_http1_http2_and_both_tls_versions_to_both_backends() {
    timeout(LONG_TIMEOUT, async {
        for backend in BACKENDS {
            with_listener(
                backend,
                vec![AlpnProtocol::H2, AlpnProtocol::Http11],
                |_| {},
                async |port| {
                    for (flags, expected) in [
                        (&["--http1.1"][..], "[200 http1.1]"),
                        (&["--http2"][..], "[200 http2]"),
                        (
                            &["--http2", "--tlsv1.2", "--tls-max", "1.2"][..],
                            "[200 http2]",
                        ),
                        (&["--http1.1", "--tlsv1.3"][..], "[200 http1.1]"),
                    ] {
                        let (success, text) = run(curl(port, flags), b"").await;
                        assert!(success, "{backend:?} curl {flags:?}: {text}");
                        assert!(
                            text.starts_with("clients"),
                            "{backend:?} curl {flags:?}: {text}"
                        );
                        assert!(
                            text.contains(expected),
                            "{backend:?} curl {flags:?}: {text}"
                        );
                    }
                },
            )
            .await;
        }
    })
    .await
    .expect("curl wire test timed out");
}

#[tokio::test]
async fn a_tls13_minimum_refuses_a_tls12_curl_on_both_backends() {
    timeout(LONG_TIMEOUT, async {
        for backend in BACKENDS {
            with_listener(
                backend,
                vec![AlpnProtocol::Http11],
                |profile| profile.min_version = TlsVersion::Tls13,
                async |port| {
                    let (success, text) =
                        run(curl(port, &["--tlsv1.2", "--tls-max", "1.2"]), b"").await;
                    assert!(!success, "{backend:?}: {text}");
                    let (success, text) = run(curl(port, &["--tlsv1.3"]), b"").await;
                    assert!(success, "{backend:?}: {text}");
                },
            )
            .await;
        }
    })
    .await
    .expect("curl version wire test timed out");
}

#[tokio::test]
async fn s_client_negotiates_alpn_versions_and_suites_with_both_backends() {
    timeout(LONG_TIMEOUT, async {
        for backend in BACKENDS {
            with_listener(
                backend,
                vec![AlpnProtocol::H2, AlpnProtocol::Http11],
                |_| {},
                async |port| {
                    // The request is HTTP/1.1 on an HTTP/2 connection: only the negotiation is looked at.
                    let (_, text) = run(s_client(port, &["-alpn", "h2,http/1.1"]), REQUEST).await;
                    assert!(text.contains("ALPN protocol: h2"), "{backend:?}: {text}");
                    assert!(
                        text.contains("New, TLSv1.3, Cipher is TLS_"),
                        "{backend:?}: {text}"
                    );
                    assert!(text.contains("Verification: OK"), "{backend:?}: {text}");

                    let (_, text) = run(s_client(port, &["-alpn", "http/1.1"]), REQUEST).await;
                    assert!(
                        text.contains("ALPN protocol: http/1.1"),
                        "{backend:?}: {text}"
                    );
                    assert!(text.contains("HTTP/1.1 200"), "{backend:?}: {text}");

                    let (_, text) = run(s_client(port, &["-alpn", "bogus"]), REQUEST).await;
                    assert!(text.contains("No ALPN negotiated"), "{backend:?}: {text}");
                    assert!(text.contains("HTTP/1.1 200"), "{backend:?}: {text}");

                    let (success, text) = run(
                        s_client(
                            port,
                            &[
                                "-tls1_2",
                                "-cipher",
                                "ECDHE-ECDSA-AES128-GCM-SHA256",
                                "-alpn",
                                "http/1.1",
                            ],
                        ),
                        REQUEST,
                    )
                    .await;
                    assert!(success, "{backend:?}: {text}");
                    assert!(
                        text.contains("New, TLSv1.2, Cipher is ECDHE-ECDSA-AES128-GCM-SHA256"),
                        "{backend:?}: {text}"
                    );
                },
            )
            .await;
        }
    })
    .await
    .expect("s_client wire test timed out");
}

/// The summary line `s_client` prints about the session of one connection, after `-sess_in` of the previous one.
async fn reuse(port: u16, version: &str, session: &Path) -> bool {
    let _ = std::fs::remove_file(session);
    let (_, first) = run(
        s_client(port, &[version, "-sess_out", session.to_str().unwrap()]),
        REQUEST,
    )
    .await;
    assert!(first.contains("New, TLSv1."), "first connection: {first}");
    if !session.exists() {
        return false;
    }
    let (success, second) = run(
        s_client(port, &[version, "-sess_in", session.to_str().unwrap()]),
        REQUEST,
    )
    .await;
    assert!(success, "second connection: {second}");
    second.contains("Reused, TLSv1.")
}

#[tokio::test]
async fn s_client_resumes_by_ticket_and_by_session_id_where_the_profile_allows_it() {
    timeout(LONG_TIMEOUT, async {
        let directory = tempfile::tempdir().expect("session directory");
        for backend in BACKENDS {
            for (tickets, cache, resumes) in [
                (true, false, true),
                (false, true, true),
                (false, false, false),
            ] {
                with_listener(
                    backend,
                    vec![AlpnProtocol::Http11],
                    |profile| {
                        profile.policy.session_tickets = tickets;
                        profile.policy.session_cache = cache.then(|| TlsSessionCache {
                            name: "clients".into(),
                            size_bytes: 1 << 20,
                        });
                    },
                    async |port| {
                        for version in ["-tls1_3", "-tls1_2"] {
                            // TLS 1.3 resumes with a stateful ticket from the cache too; a client only finds a ticket
                            // to resume when the server sent one.
                            let reused =
                                reuse(port, version, &directory.path().join("session")).await;
                            assert_eq!(
                                reused, resumes,
                                "{backend:?} {version} tickets {tickets} cache {cache}"
                            );
                        }
                    },
                )
                .await;
            }
        }
    })
    .await
    .expect("resumption wire test timed out");
}

#[tokio::test]
async fn the_ticket_lifetime_hint_is_the_session_timeout_on_both_backends() {
    timeout(LONG_TIMEOUT, async {
        for backend in BACKENDS {
            for (configured, expected) in [(None, 7_200), (Some(120), 120)] {
                with_listener(
                    backend,
                    vec![AlpnProtocol::Http11],
                    |profile| {
                        profile.policy.session_tickets = true;
                        profile.policy.session_timeout_seconds = configured;
                    },
                    async |port| {
                        for version in ["-tls1_3", "-tls1_2"] {
                            let (success, text) = run(s_client(port, &[version]), REQUEST).await;
                            assert!(success, "{backend:?} {version}: {text}");
                            assert!(
                                text.contains(&format!("lifetime hint: {expected} (seconds)")),
                                "{backend:?} {version} timeout {configured:?}: {text}"
                            );
                        }
                    },
                )
                .await;
            }
        }
    })
    .await
    .expect("ticket lifetime wire test timed out");
}

#[tokio::test]
async fn client_certificates_work_with_curl_and_s_client_on_both_backends() {
    timeout(LONG_TIMEOUT, async {
        let client = generate_test_only_client_chain("client.example.test");
        let stranger = generate_test_only_client_chain("client.example.test");
        for backend in BACKENDS {
            let ca = client.root_certificate_path.clone();
            with_listener(
                backend,
                vec![AlpnProtocol::Http11],
                |profile| {
                    profile.policy.client_auth = TlsClientAuthPolicy {
                        mode: TlsClientAuthMode::Required,
                        ca_certificate_path: Some(ca),
                        allowed_dns_names: vec!["client.example.test".into()],
                    };
                },
                async |port| {
                    let identity = |chain: &Path, key: &Path| {
                        [
                            "--cert".to_owned(),
                            chain.display().to_string(),
                            "--key".to_owned(),
                            key.display().to_string(),
                        ]
                    };
                    let good = identity(&client.fullchain_path, &client.leaf_private_key_path);
                    let good = good.iter().map(String::as_str).collect::<Vec<_>>();
                    let (success, text) = run(curl(port, &good), b"").await;
                    assert!(
                        success && text.contains("[200 http1.1]"),
                        "{backend:?}: {text}"
                    );

                    let (success, text) = run(curl(port, &[]), b"").await;
                    assert!(!success, "{backend:?} accepted no certificate: {text}");

                    let other = identity(&stranger.fullchain_path, &stranger.leaf_private_key_path);
                    let other = other.iter().map(String::as_str).collect::<Vec<_>>();
                    let (success, text) = run(curl(port, &other), b"").await;
                    assert!(
                        !success,
                        "{backend:?} accepted a certificate of another CA: {text}"
                    );

                    let (_, text) = run(
                        s_client(
                            port,
                            &[
                                "-cert",
                                client.fullchain_path.to_str().unwrap(),
                                "-key",
                                client.leaf_private_key_path.to_str().unwrap(),
                                "-cert_chain",
                                client.intermediate_certificate_path.to_str().unwrap(),
                            ],
                        ),
                        REQUEST,
                    )
                    .await;
                    assert!(text.contains("HTTP/1.1 200"), "{backend:?}: {text}");
                },
            )
            .await;
        }
    })
    .await
    .expect("client certificate wire test timed out");
}
