use std::fmt::Write;

use pingora::protocols::tls::{ALPN, CustomALPN, SslDigest};
use rustls::{ProtocolVersion, ServerConnection};
use sha2::{Digest, Sha256};
use x509_parser::parse_x509_certificate;

/// What the connection negotiated, as the digest the HTTP layers expect. The peer certificate is read only when the
/// client sent one.
pub(crate) fn ssl_digest(conn: &ServerConnection) -> SslDigest {
    let cipher = conn
        .negotiated_cipher_suite()
        .and_then(|suite| suite.suite().as_str())
        .map_or("", openssl_suite_name);
    let version = match conn.protocol_version() {
        Some(ProtocolVersion::TLSv1_3) => "TLSv1.3",
        Some(ProtocolVersion::TLSv1_2) => "TLSv1.2",
        _ => "",
    };
    let Some(leaf) = conn.peer_certificates().and_then(<[_]>::first) else {
        return SslDigest::new(cipher, version, None, None, Vec::new());
    };
    let (organization, serial) =
        parse_x509_certificate(leaf.as_ref()).map_or((None, None), |(_, certificate)| {
            let organization = certificate
                .subject()
                .iter_organization()
                .next()
                .and_then(|attribute| attribute.as_str().ok())
                .map(str::to_owned);
            (organization, Some(hex(certificate.raw_serial())))
        });
    SslDigest::new(
        cipher,
        version,
        organization,
        serial,
        Sha256::digest(leaf.as_ref()).to_vec(),
    )
}

/// The name OpenSSL (and so nginx, curl and the OpenSSL backend's digest) gives a suite that rustls names by its IANA
/// identifier; a suite OpenSSL has no name for keeps rustls' name.
#[must_use]
pub fn openssl_suite_name(name: &'static str) -> &'static str {
    match name {
        "TLS13_AES_128_GCM_SHA256" => "TLS_AES_128_GCM_SHA256",
        "TLS13_AES_256_GCM_SHA384" => "TLS_AES_256_GCM_SHA384",
        "TLS13_CHACHA20_POLY1305_SHA256" => "TLS_CHACHA20_POLY1305_SHA256",
        "TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256" => "ECDHE-ECDSA-AES128-GCM-SHA256",
        "TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256" => "ECDHE-RSA-AES128-GCM-SHA256",
        "TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384" => "ECDHE-ECDSA-AES256-GCM-SHA384",
        "TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384" => "ECDHE-RSA-AES256-GCM-SHA384",
        "TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256" => "ECDHE-ECDSA-CHACHA20-POLY1305",
        "TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256" => "ECDHE-RSA-CHACHA20-POLY1305",
        other => other,
    }
}

pub(crate) fn selected_alpn(conn: &ServerConnection) -> Option<ALPN> {
    conn.alpn_protocol().map(|protocol| match protocol {
        b"http/1.1" => ALPN::H1,
        b"h2" => ALPN::H2,
        other => ALPN::Custom(CustomALPN::new(other.to_vec())),
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02X}");
        hex
    })
}
