use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
};

use oxiroute_forward_proxy::H3_ALPN;
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rcgen::{CertifiedKey, generate_simple_self_signed};
use rustls::pki_types::PrivatePkcs8KeyDer;

pub struct H3Endpoints {
    pub server: quinn::Endpoint,
    pub client: quinn::Endpoint,
    pub server_address: SocketAddr,
}

pub fn endpoints() -> H3Endpoints {
    endpoints_with_alpn(vec![H3_ALPN.to_vec()], vec![H3_ALPN.to_vec()])
}

pub fn endpoints_with_alpn(server_alpn: Vec<Vec<u8>>, client_alpn: Vec<Vec<u8>>) -> H3Endpoints {
    let CertifiedKey { cert, key_pair } =
        generate_simple_self_signed(vec!["localhost".into()]).expect("test certificate");
    let certificate = cert.der().clone();
    let key = PrivatePkcs8KeyDer::from(key_pair.serialize_der());

    let mut server_crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![certificate.clone()], key.into())
        .expect("server TLS config");
    server_crypto.alpn_protocols = server_alpn;
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(
        QuicServerConfig::try_from(server_crypto).expect("QUIC server crypto"),
    ));
    let server = quinn::Endpoint::server(server_config, SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .expect("QUIC server endpoint");
    let server_address = server.local_addr().expect("QUIC server address");

    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate).expect("test root certificate");
    let mut client_crypto = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    client_crypto.alpn_protocols = client_alpn;
    let client_config = quinn::ClientConfig::new(Arc::new(
        QuicClientConfig::try_from(client_crypto).expect("QUIC client crypto"),
    ));
    let mut client = quinn::Endpoint::client(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .expect("QUIC client endpoint");
    client.set_default_client_config(client_config);

    H3Endpoints {
        server,
        client,
        server_address,
    }
}
