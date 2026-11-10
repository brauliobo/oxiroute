//! The rustls crypto providers of the process. Both are named at every use: the build links `ring` and `aws-lc-rs`, so
//! rustls has no implicit default.

use std::sync::{Arc, OnceLock};

use rustls::crypto::{CryptoProvider, aws_lc_rs, ring};

/// The provider of HTTP/3 and its QUIC handshakes.
pub(crate) fn ring_provider() -> Arc<CryptoProvider> {
    static PROVIDER: OnceLock<Arc<CryptoProvider>> = OnceLock::new();
    Arc::clone(PROVIDER.get_or_init(|| Arc::new(ring::default_provider())))
}

/// The provider of the rustls TLS backend. Its suites keep the order of Mozilla's intermediate profile, which the
/// OpenSSL listeners use: AES-128-GCM first, so the SHA-256 transcript is the common case and the 3x costlier SHA-384
/// handshake is only chosen when the client asks for it.
pub(crate) fn aws_lc_provider() -> Arc<CryptoProvider> {
    static PROVIDER: OnceLock<Arc<CryptoProvider>> = OnceLock::new();
    Arc::clone(PROVIDER.get_or_init(|| {
        use aws_lc_rs::cipher_suite::{
            TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256, TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
            TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256, TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
            TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384, TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
            TLS13_AES_128_GCM_SHA256, TLS13_AES_256_GCM_SHA384, TLS13_CHACHA20_POLY1305_SHA256,
        };
        Arc::new(CryptoProvider {
            cipher_suites: vec![
                TLS13_AES_128_GCM_SHA256,
                TLS13_AES_256_GCM_SHA384,
                TLS13_CHACHA20_POLY1305_SHA256,
                TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
                TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
                TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
                TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
                TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
                TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
            ],
            ..aws_lc_rs::default_provider()
        })
    }))
}
