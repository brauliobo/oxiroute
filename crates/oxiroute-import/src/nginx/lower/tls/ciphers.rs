use oxiroute_config::{CipherList, TlsVersion};

use crate::{Diagnostic, DiagnosticCode, nginx::DirectiveOrigin};

use super::super::provenance::warning;

const W_TLS_CIPHERS_DEFAULTED: DiagnosticCode = DiagnosticCode::new("W_TLS_CIPHERS_DEFAULTED");
const W_TLS_CIPHERS_LEFT_OUT: DiagnosticCode = DiagnosticCode::new("W_TLS_CIPHERS_LEFT_OUT");
const W_TLS_BACKEND_OPENSSL: DiagnosticCode = DiagnosticCode::new("W_TLS_BACKEND_OPENSSL");

const LEGACY_RECIPE: &str = "serve the clients that need them from a separate listener and TLS profile with `backend = \"openssl\"` and an explicit `cipher_list` naming the suites; see \"Serving legacy clients\" in docs/OPERATIONS.md and the `policy.backend` entry of docs/CONFIG_SPEC.md";

#[derive(Debug, Eq, PartialEq)]
enum Outcome {
    Kept,
    Defaulted,
    DheRsa,
    NoRustlsSuite,
    LeftOut(Vec<String>),
}

impl Outcome {
    fn of(list: &CipherList<'_>, openssl_backend: bool) -> Self {
        if openssl_backend {
            Self::Kept
        } else if list.lists_dhe_rsa() {
            Self::DheRsa
        } else if list.has_macros() {
            Self::Defaulted
        } else if !list.lists_rustls_tls12_suite() {
            Self::NoRustlsSuite
        } else {
            Self::left_out(list)
        }
    }

    fn left_out(list: &CipherList<'_>) -> Self {
        let suites = list
            .suites_rustls_cannot_serve()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if suites.is_empty() {
            Self::Kept
        } else {
            Self::LeftOut(suites)
        }
    }
}

pub(super) struct CipherImport {
    value: String,
    origin: DirectiveOrigin,
    outcome: Outcome,
}

impl CipherImport {
    pub(super) fn new(value: &str, origin: &DirectiveOrigin, openssl_backend: bool) -> Self {
        Self {
            value: value.to_owned(),
            origin: origin.clone(),
            outcome: Outcome::of(&CipherList::parse(value), openssl_backend),
        }
    }

    pub(super) fn cipher_list(&self) -> Option<String> {
        (self.outcome != Outcome::Defaulted).then(|| self.value.clone())
    }

    fn diagnostic(&self) -> Option<Diagnostic> {
        let (code, message) = match &self.outcome {
            Outcome::Kept => return None,
            Outcome::Defaulted => (W_TLS_CIPHERS_DEFAULTED, self.defaulted_message()),
            Outcome::DheRsa => (
                W_TLS_BACKEND_OPENSSL,
                format!(
                    "ssl_ciphers `{}` lists DHE-RSA suites, which only the OpenSSL backend serves: the list is imported unchanged and the TLS profile runs on the OpenSSL backend",
                    self.value
                ),
            ),
            Outcome::NoRustlsSuite => (
                W_TLS_BACKEND_OPENSSL,
                format!(
                    "ssl_ciphers `{}` lists no TLS 1.2 suite the rustls backend implements: the list is imported unchanged and the TLS profile runs on the OpenSSL backend",
                    self.value
                ),
            ),
            Outcome::LeftOut(suites) => (
                W_TLS_CIPHERS_LEFT_OUT,
                format!(
                    "ssl_ciphers `{}` lists suites the rustls backend does not implement ({}): the list is imported unchanged and the TLS profile stays on rustls, which does not offer them. To serve them, set `backend = \"openssl\"` on the profile, or {LEGACY_RECIPE}",
                    self.value,
                    suites.join(", ")
                ),
            ),
        };
        Some(warning(&self.origin, code, message))
    }

    fn defaulted_message(&self) -> String {
        format!(
            "ssl_ciphers `{}` is an OpenSSL cipher macro that the rustls TLS backend cannot interpret: it is replaced by the profile's default cipher policy, the modern list served by rustls (TLS 1.3 AES-GCM and ChaCha20-Poly1305, TLS 1.2 ECDHE AEAD suites). DHE-RSA, CBC and RSA key-exchange suites are not offered; to keep them, {LEGACY_RECIPE}",
            self.value
        )
    }
}

#[derive(Default)]
pub(super) struct PolicyNotes {
    pub(super) ciphers: Option<CipherImport>,
    pub(super) dhparam: Option<DirectiveOrigin>,
}

impl PolicyNotes {
    pub(super) fn diagnostics(&self, minimum: TlsVersion) -> Vec<Diagnostic> {
        let ciphers = self
            .ciphers
            .as_ref()
            .filter(|_| minimum == TlsVersion::Tls12)
            .and_then(CipherImport::diagnostic);
        let dhparam = self.dhparam.as_ref().map(|origin| {
            warning(
                origin,
                W_TLS_BACKEND_OPENSSL,
                "ssl_dhparam is imported as `dh_parameters_path`, which only the OpenSSL backend serves: the TLS profile runs on the OpenSSL backend",
            )
        });
        ciphers.into_iter().chain(dhparam).collect()
    }
}
