//! The rustls crypto providers of the process. Both are named at every use: the build links `ring` and `aws-lc-rs`, so
//! rustls has no implicit default.

use std::sync::{Arc, OnceLock};

use oxiroute_config::TlsKeyExchangeGroup;
use oxiroute_tls_rustls::openssl_suite_name;
use rustls::{
    NamedGroup, SupportedCipherSuite,
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

/// `provider` with the TLS 1.2 suites that OpenSSL enables for an OpenSSL cipher string (`selected`, in OpenSSL's
/// order), and the names among them that the provider cannot implement (finite-field DHE, CBC and RSA key exchange
/// suites). The TLS 1.3 suites stay as they are: an OpenSSL cipher string does not govern them.
pub(crate) fn with_cipher_list(
    provider: &CryptoProvider,
    selected: &[String],
) -> (Arc<CryptoProvider>, Vec<String>) {
    let (mut cipher_suites, tls12): (Vec<_>, Vec<_>) = provider
        .cipher_suites
        .iter()
        .copied()
        .partition(|suite| matches!(suite, SupportedCipherSuite::Tls13(_)));
    let mut left_out = Vec::new();
    for name in selected.iter().filter(|name| !name.starts_with("TLS_")) {
        let suite = tls12
            .iter()
            .find(|suite| suite.suite().as_str().map(openssl_suite_name) == Some(name.as_str()));
        match suite {
            Some(&suite) => cipher_suites.push(suite),
            None => left_out.push(name.clone()),
        }
    }
    let provider = CryptoProvider {
        cipher_suites,
        ..provider.clone()
    };
    (Arc::new(provider), left_out)
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

    fn tls12_names(provider: &CryptoProvider) -> Vec<&'static str> {
        provider
            .cipher_suites
            .iter()
            .filter(|suite| matches!(suite, SupportedCipherSuite::Tls12(_)))
            .filter_map(|suite| suite.suite().as_str().map(openssl_suite_name))
            .collect()
    }

    #[test]
    fn the_cipher_list_orders_the_tls12_suites_and_names_what_the_provider_lacks() {
        let selected = [
            "TLS_AES_256_GCM_SHA384",
            "ECDHE-RSA-CHACHA20-POLY1305",
            "DHE-RSA-AES128-GCM-SHA256",
            "ECDHE-ECDSA-AES128-GCM-SHA256",
            "AES128-SHA",
        ]
        .map(String::from);

        let (provider, left_out) = with_cipher_list(&aws_lc_provider(), &selected);

        assert_eq!(
            tls12_names(&provider),
            [
                "ECDHE-RSA-CHACHA20-POLY1305",
                "ECDHE-ECDSA-AES128-GCM-SHA256"
            ]
        );
        assert_eq!(left_out, ["DHE-RSA-AES128-GCM-SHA256", "AES128-SHA"]);
        let tls13 = provider
            .cipher_suites
            .iter()
            .filter(|suite| matches!(suite, SupportedCipherSuite::Tls13(_)))
            .count();
        assert_eq!(tls13, 3);
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
