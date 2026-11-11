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
        .map_or("", openssl_name);
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

/// TLS 1.3 suites carry the names OpenSSL reports; the TLS 1.2 suites keep their IANA names.
fn openssl_name(name: &'static str) -> &'static str {
    match name {
        "TLS13_AES_128_GCM_SHA256" => "TLS_AES_128_GCM_SHA256",
        "TLS13_AES_256_GCM_SHA384" => "TLS_AES_256_GCM_SHA384",
        "TLS13_CHACHA20_POLY1305_SHA256" => "TLS_CHACHA20_POLY1305_SHA256",
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
