use oxiroute_config::{CipherList, TlsBackend, TlsProfile, TlsVersion};
use pingora::listeners::tls::cipher_list_names;
use rustls::SupportedCipherSuite;

use super::TlsBuildError;
use crate::tls::provider::{aws_lc_provider, with_cipher_list};

const DH_PARAMETERS: &str = "policy.dh_parameters_path";
const CIPHER_LIST: &str = "policy.cipher_list";

pub(super) fn select_backend(profile: &TlsProfile) -> Result<TlsBackend, TlsBuildError> {
    match profile.policy.backend {
        Some(TlsBackend::Openssl) => Ok(TlsBackend::Openssl),
        Some(TlsBackend::Rustls) => reject_openssl_only(profile).map(|()| TlsBackend::Rustls),
        None => default_backend(profile),
    }
}

fn default_backend(profile: &TlsProfile) -> Result<TlsBackend, TlsBuildError> {
    let Some(setting) = openssl_only_setting(profile)? else {
        return Ok(TlsBackend::default());
    };
    log::info!(
        "TLS profile `{}`: the OpenSSL backend is selected because `{setting}` needs it",
        profile.name
    );
    Ok(TlsBackend::Openssl)
}

fn reject_openssl_only(profile: &TlsProfile) -> Result<(), TlsBuildError> {
    match openssl_only_setting(profile)? {
        Some(setting) => Err(TlsBuildError::BackendNeedsOpenssl {
            profile: profile.name.clone(),
            setting,
        }),
        None => Ok(()),
    }
}

fn openssl_only_setting(profile: &TlsProfile) -> Result<Option<&'static str>, TlsBuildError> {
    if profile.policy.dh_parameters_path.is_some() {
        return Ok(Some(DH_PARAMETERS));
    }
    let Some(cipher_list) = &profile.policy.cipher_list else {
        return Ok(None);
    };
    let needs_openssl = profile.min_version == TlsVersion::Tls12
        && (CipherList::parse(cipher_list).lists_dhe_rsa()
            || selects_no_rustls_suite(&profile.name, cipher_list)?);
    Ok(needs_openssl.then_some(CIPHER_LIST))
}

fn selects_no_rustls_suite(profile: &str, cipher_list: &str) -> Result<bool, TlsBuildError> {
    let selected =
        cipher_list_names(cipher_list).map_err(|source| TlsBuildError::TlsProfileSettings {
            profile: profile.to_owned(),
            source,
        })?;
    let (provider, _) = with_cipher_list(&aws_lc_provider(), &selected);
    Ok(!provider
        .cipher_suites
        .iter()
        .any(|suite| matches!(suite, SupportedCipherSuite::Tls12(_))))
}
