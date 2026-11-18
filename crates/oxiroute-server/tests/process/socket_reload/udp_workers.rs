use std::{collections::BTreeSet, fs, net::SocketAddr, path::Path, sync::Arc, time::Duration};

use oxiroute_config::{
    AlpnProtocol, Certificate, CertificateSource, ConfigDraft, ListenerBind, Management, Protocol,
    TlsPolicy, TlsProfile, TlsVersion,
};
use quinn::crypto::rustls::QuicClientConfig;
use rustls::pki_types::{CertificateDer, pem::PemObject};

use super::{
    Instance, WAIT, config_support::empty_config, fixed_service, fixture_support, listener,
    reserve_tcp_address, runtime_modes, wait_until,
};

fn http3_config(key: &Path, workers: u64) -> (ConfigDraft, SocketAddr, SocketAddr) {
    let (management, address) = (reserve_tcp_address(), reserve_tcp_address());
    let mut config = empty_config();
    config.listener_workers = Some(workers);
    config.management = Some(Management {
        bind: management,
        ui_dir: None,
        token_file: None,
    });
    config.certificates = vec![Certificate {
        name: "downstream".into(),
        dns_names: vec!["proxy.example.test".into()],
        source: CertificateSource::Files {
            certificate_chain_path: fixture_support::fixture("proxy-a.pem"),
            private_key_path: key.to_owned(),
        },
    }];
    config.tls_profiles = vec![TlsProfile {
        name: "downstream".into(),
        certificates: vec!["downstream".into()],
        default_certificate: "downstream".into(),
        min_version: TlsVersion::Tls13,
        alpn: vec![AlpnProtocol::H3],
        policy: TlsPolicy::default(),
    }];
    config.listeners = vec![oxiroute_config::Listener {
        protocol: Protocol::Http3,
        tls_profile: Some("downstream".into()),
        ..listener("h3", ListenerBind::Udp { address })
    }];
    config.http_services = vec![fixed_service("h3")];
    (config, management, address)
}

fn udp_sockets_on(port: u16) -> usize {
    ["/proc/net/udp", "/proc/net/udp6"]
        .into_iter()
        .flat_map(|table| {
            fs::read_to_string(table)
                .expect("socket table")
                .lines()
                .skip(1)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .filter_map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            let local_port = u16::from_str_radix(fields.get(1)?.rsplit(':').next()?, 16).ok()?;
            (local_port == port).then(|| fields[9].to_owned())
        })
        .collect::<BTreeSet<_>>()
        .len()
}

fn client_endpoint() -> quinn::Endpoint {
    let mut roots = rustls::RootCertStore::empty();
    for certificate in CertificateDer::pem_slice_iter(
        &fs::read(fixture_support::fixture("ca-a.pem")).expect("CA fixture"),
    ) {
        roots
            .add(certificate.expect("CA certificate"))
            .expect("trusted CA");
    }
    let mut crypto = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("protocol versions")
    .with_root_certificates(roots)
    .with_no_client_auth();
    crypto.alpn_protocols = vec![b"h3".to_vec()];
    let mut endpoint = quinn::Endpoint::client((std::net::Ipv4Addr::LOCALHOST, 0).into())
        .expect("client endpoint");
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(
        QuicClientConfig::try_from(crypto).expect("QUIC client config"),
    )));
    endpoint
}

async fn handshakes(address: SocketAddr, clients: usize) {
    for _ in 0..clients {
        let endpoint = client_endpoint();
        let connection = tokio::time::timeout(WAIT, async {
            endpoint
                .connect(address, "proxy.example.test")
                .expect("connect")
                .await
        })
        .await
        .expect("handshake arrives")
        .expect("handshake completes");
        connection.close(0_u32.into(), b"done");
        endpoint.wait_idle().await;
    }
}

#[tokio::test]
async fn a_kept_http3_socket_runs_the_new_worker_count_after_a_reload() {
    for supervised in runtime_modes() {
        let label = if supervised { "supervised" } else { "direct" };
        let key = fixture_support::private_key_fixture("proxy-a-key.pem");
        let (config, management, address) = http3_config(key.path(), 1);
        let mut instance = Instance::start((config, management, management), supervised).await;
        wait_until("the UDP socket", || async {
            (udp_sockets_on(address.port()) == 1).then_some(())
        })
        .await;
        handshakes(address, 4).await;

        instance
            .apply(|config| config.listener_workers = Some(3))
            .await;
        wait_until("three UDP sockets on the kept address", || async {
            (udp_sockets_on(address.port()) == 3).then_some(())
        })
        .await;
        assert_eq!(udp_sockets_on(address.port()), 3, "{label}");
        handshakes(address, 24).await;

        instance.stop();
    }
}
