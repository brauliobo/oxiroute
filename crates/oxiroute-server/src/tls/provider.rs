//! The rustls crypto providers of the process. Both are named at every use: the build links `ring` and `aws-lc-rs`, so
//! rustls has no implicit default.

use std::sync::{Arc, OnceLock};

use oxiroute_config::TlsKeyExchangeGroup;
use rustls::{
    NamedGroup,
    crypto::{CryptoProvider, aws_lc_rs, ring},
};

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

/// `provider` restricted to the profile's key-exchange groups, in the profile's order: the TLS 1.3 key shares and the
/// TLS 1.2 ECDHE curves it accepts, and the group it asks a client to retry with. A listed group the provider cannot
/// implement is skipped; `None` keeps the provider's own list.
///
/// # Errors
///
/// Returns an error when none of the listed groups is available.
pub(crate) fn with_key_exchange_groups(
    provider: Arc<CryptoProvider>,
    groups: Option<&[TlsKeyExchangeGroup]>,
) -> Result<Arc<CryptoProvider>, &'static str> {
    let Some(groups) = groups else {
        return Ok(provider);
    };
    let kx_groups: Vec<_> = groups
        .iter()
        .filter_map(|&group| {
            provider
                .kx_groups
                .iter()
                .copied()
                .find(|kx| kx.name() == named_group(group))
        })
        .collect();
    if kx_groups.is_empty() {
        return Err("no key_exchange_groups entry is available to the TLS provider");
    }
    Ok(Arc::new(CryptoProvider {
        kx_groups,
        ..(*provider).clone()
    }))
}

const fn named_group(group: TlsKeyExchangeGroup) -> NamedGroup {
    match group {
        TlsKeyExchangeGroup::X25519 => NamedGroup::X25519,
        TlsKeyExchangeGroup::Secp256r1 => NamedGroup::secp256r1,
        TlsKeyExchangeGroup::Secp384r1 => NamedGroup::secp384r1,
        TlsKeyExchangeGroup::X25519Mlkem768 => NamedGroup::X25519MLKEM768,
    }
}

#[cfg(test)]
mod tests {
    use TlsKeyExchangeGroup::{Secp256r1, X25519, X25519Mlkem768};

    use super::*;

    fn names(provider: &CryptoProvider) -> Vec<NamedGroup> {
        provider.kx_groups.iter().map(|kx| kx.name()).collect()
    }

    #[test]
    fn the_listed_groups_replace_the_default_list_in_their_order() {
        let provider =
            with_key_exchange_groups(aws_lc_provider(), Some(&[Secp256r1, X25519])).unwrap();

        assert_eq!(
            names(&provider),
            [NamedGroup::secp256r1, NamedGroup::X25519]
        );
        assert_eq!(
            names(&with_key_exchange_groups(aws_lc_provider(), None).unwrap())[0],
            NamedGroup::X25519MLKEM768
        );
    }

    #[test]
    fn a_group_the_provider_lacks_is_skipped_and_an_empty_result_is_an_error() {
        let provider =
            with_key_exchange_groups(ring_provider(), Some(&[X25519Mlkem768, X25519])).unwrap();

        assert_eq!(names(&provider), [NamedGroup::X25519]);
        assert!(with_key_exchange_groups(ring_provider(), Some(&[X25519Mlkem768])).is_err());
        assert!(with_key_exchange_groups(aws_lc_provider(), Some(&[X25519Mlkem768])).is_ok());
    }
}
