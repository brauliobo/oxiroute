//! The rustls form of a TLS profile: one set of connection configurations per listener, a resolver that selects the
//! active certificate generation by SNI, and the router that picks between them from the `ClientHello`.

use std::sync::Arc;

use oxiroute_tls_rustls::RustlsAcceptor;
use pingora::listeners::{CustomTlsSettings, tls::cipher_list_names};
use rustls::{
    ServerConfig, SupportedCipherSuite, SupportedProtocolVersion,
    crypto::{CryptoProvider, aws_lc_rs::Ticketer},
    server::{NoServerSessionStorage, ProducesTickets, StoresServerSessions},
    version,
};

use self::{
    resolver::GenerationResolver,
    router::ProfileRouter,
    session::{SessionCache, TimedTicketer},
};
use super::{ALPN, TLS13_TICKETS, TlsBackend, TlsBuildError, TlsProfilePlan, TlsVersion};
use crate::tls::provider::{aws_lc_provider, with_cipher_list, with_key_exchange_groups};

mod resolver;
mod router;
mod session;

/// OpenSSL's default session timeout for TLS 1.3, which a profile without `session_timeout_seconds` has always had
/// (it is 300 seconds for TLS 1.2 sessions; the stamp inside a rustls ticket cannot tell the versions apart).
const DEFAULT_SESSION_TIMEOUT_SECONDS: u32 = 7_200;

impl TlsProfilePlan {
    /// Settings that hand the accepted connections of a listener to the rustls backend.
    ///
    /// # Errors
    ///
    /// Returns an error if the profile asks for the OpenSSL backend, a certificate identity cannot be used by rustls,
    /// or rustls rejects the profile's protocol or session settings.
    pub fn rustls_settings(
        &self,
        handshake_threads: usize,
    ) -> Result<CustomTlsSettings, TlsBuildError> {
        if self.backend != TlsBackend::Rustls {
            return Err(self.rustls_error("the profile selects the OpenSSL backend".into()));
        }
        for (name, active) in &self.selector.active_generations {
            if let Err(detail) = active.snapshot().certified_key() {
                return Err(self.rustls_error(format!("certificate `{name}`: {detail}")));
            }
        }
        if self.dh_parameters.is_some() {
            log::warn!(
                "TLS profile `{}`: dh_parameters_path is not used by the rustls backend, which offers no finite-field DHE suites",
                self.name
            );
        }
        let protocols = alpn_protocols(&self.alpn);
        let provider = self.rustls_provider()?;
        let sessions = self.shared_sessions()?;
        let strict = self.rustls_server_config(&provider, protocols.clone(), &sessions)?;
        // A client that offers protocols the profile does not speak is served without ALPN, as the OpenSSL backend
        // does, unless the profile speaks HTTP/2 only.
        let lenient = if matches!(self.alpn, ALPN::H2) {
            None
        } else {
            Some(self.rustls_server_config(&provider, Vec::new(), &sessions)?)
        };
        let router = ProfileRouter::new(
            strict,
            lenient,
            protocols,
            self.tls_alpn_challenge_store.clone(),
            self.protocol_versions(),
        );
        let mut settings = CustomTlsSettings::new(Arc::new(RustlsAcceptor::new(Arc::new(router))));
        if handshake_threads > 0 {
            settings.set_offload_threadpool(handshake_threads, 1);
        }
        Ok(settings)
    }

    fn protocol_versions(&self) -> &'static [&'static SupportedProtocolVersion] {
        static TLS12_AND_TLS13: [&SupportedProtocolVersion; 2] = [&version::TLS13, &version::TLS12];
        static TLS13_ONLY: [&SupportedProtocolVersion; 1] = [&version::TLS13];
        match self.min_version {
            TlsVersion::Tls12 => &TLS12_AND_TLS13,
            TlsVersion::Tls13 => &TLS13_ONLY,
        }
    }

    /// The crypto provider of the profile: the key exchange groups and, for the TLS 1.2 suites, the `cipher_list` the
    /// profile asks for, evaluated by OpenSSL so the string means what it means on the OpenSSL backend. The suites the
    /// rustls provider cannot implement are named once in the log.
    fn rustls_provider(&self) -> Result<Arc<CryptoProvider>, TlsBuildError> {
        let provider = with_key_exchange_groups(
            aws_lc_provider(),
            self.policy.key_exchange_groups.as_deref(),
        )
        .map_err(|detail| self.rustls_error(detail.into()))?;
        let Some(cipher_list) = &self.cipher_list else {
            return Ok(provider);
        };
        let selected = cipher_list_names(cipher_list)
            .map_err(|error| self.rustls_error(format!("cipher_list: {error}")))?;
        let (provider, left_out) = with_cipher_list(&provider, &selected);
        if !left_out.is_empty() {
            let shown = left_out
                .iter()
                .take(6)
                .map(String::as_str)
                .collect::<Vec<_>>();
            log::warn!(
                "TLS profile `{}`: cipher_list selects {} TLS 1.2 suites the rustls backend does not implement and leaves out: {}{}",
                self.name,
                left_out.len(),
                shown.join(", "),
                if left_out.len() > shown.len() {
                    ", ..."
                } else {
                    ""
                }
            );
        }
        let tls12 = provider
            .cipher_suites
            .iter()
            .any(|suite| matches!(suite, SupportedCipherSuite::Tls12(_)));
        if self.min_version == TlsVersion::Tls12 && !tls12 {
            return Err(self.rustls_error(
                "cipher_list selects no TLS 1.2 suite the rustls backend implements (ECDHE with AES-GCM or ChaCha20-Poly1305)"
                    .into(),
            ));
        }
        Ok(provider)
    }

    fn rustls_server_config(
        &self,
        provider: &Arc<CryptoProvider>,
        alpn_protocols: Vec<Vec<u8>>,
        sessions: &Sessions,
    ) -> Result<Arc<ServerConfig>, TlsBuildError> {
        let builder = ServerConfig::builder_with_provider(Arc::clone(provider))
            .with_protocol_versions(self.protocol_versions())
            .map_err(|error| self.rustls_error(error.to_string()))?;
        let builder = match self.client_auth.rustls_verifier() {
            Some(verifier) => builder.with_client_cert_verifier(verifier),
            None => builder.with_no_client_auth(),
        };
        let resolver = GenerationResolver::new(self.name.clone(), Arc::clone(&self.selector));
        let mut config = builder.with_cert_resolver(Arc::new(resolver));
        config.alpn_protocols = alpn_protocols;
        config.ignore_client_order = self.prefer_server_ciphers;
        config.session_storage = Arc::clone(&sessions.storage);
        if let Some(tickets) = &sessions.tickets {
            config.ticketer = Arc::clone(tickets);
        }
        config.send_tls13_tickets = sessions.tls13_tickets;
        Ok(Arc::new(config))
    }

    /// Resumption as the profile declares it: a named cache keeps sessions in memory (TLS 1.2 ids and TLS 1.3 stateful
    /// tickets), stateless tickets are encrypted by per-process keys, and a TLS 1.3 ticket nobody could redeem is not
    /// sent. Every configuration of the profile shares the one state, so a session resumes whichever serves it and
    /// whichever listener of the profile accepts the next connection.
    fn shared_sessions(&self) -> Result<Arc<Sessions>, TlsBuildError> {
        if let Some(sessions) = self.rustls_sessions.get() {
            return Ok(Arc::clone(sessions));
        }
        let sessions = Arc::new(self.sessions()?);
        Ok(Arc::clone(self.rustls_sessions.get_or_init(|| sessions)))
    }

    fn sessions(&self) -> Result<Sessions, TlsBuildError> {
        let timeout = self
            .session_timeout
            .map_or(DEFAULT_SESSION_TIMEOUT_SECONDS, i32::unsigned_abs);
        let storage: Arc<dyn StoresServerSessions> = match &self.session_cache {
            Some(cache) => Arc::new(SessionCache::new(
                usize::try_from(cache.capacity).unwrap_or_default(),
                timeout,
            )),
            None => Arc::new(NoServerSessionStorage {}),
        };
        let tickets: Option<Arc<dyn ProducesTickets>> = if self.session_tickets {
            let keys = Ticketer::new().map_err(|error| self.rustls_error(error.to_string()))?;
            Some(Arc::new(TimedTicketer::new(keys, timeout)))
        } else {
            None
        };
        let resumable = self.session_tickets || self.session_cache.is_some();
        Ok(Sessions {
            storage,
            tickets,
            tls13_tickets: if resumable { TLS13_TICKETS } else { 0 },
        })
    }

    fn rustls_error(&self, detail: String) -> TlsBuildError {
        TlsBuildError::RustlsSettings {
            profile: self.name.clone(),
            detail,
        }
    }
}

/// The resumption state the configurations of one profile share.
pub(super) struct Sessions {
    storage: Arc<dyn StoresServerSessions>,
    tickets: Option<Arc<dyn ProducesTickets>>,
    tls13_tickets: usize,
}

fn alpn_protocols(alpn: &ALPN) -> Vec<Vec<u8>> {
    match alpn {
        ALPN::H1 => vec![b"http/1.1".to_vec()],
        ALPN::H2 => vec![b"h2".to_vec()],
        ALPN::H2H1 => vec![b"h2".to_vec(), b"http/1.1".to_vec()],
        ALPN::Custom(custom) => vec![custom.protocol().to_vec()],
    }
}
