//! The rustls form of a TLS profile: one set of connection configurations per listener, a resolver that selects the
//! active certificate generation by SNI, and the router that picks between them from the ClientHello.

use std::sync::Arc;

use oxiroute_tls_rustls::RustlsAcceptor;
use pingora::listeners::CustomTlsSettings;
use rustls::{
    ServerConfig, SupportedProtocolVersion,
    crypto::aws_lc_rs::Ticketer,
    server::{NoServerSessionStorage, ProducesTickets, StoresServerSessions},
    version,
};

use self::{
    resolver::GenerationResolver,
    router::ProfileRouter,
    session::{SessionCache, TimedTicketer},
};
use super::{ALPN, TlsBackend, TlsBuildError, TlsProfilePlan, TlsVersion};

mod resolver;
mod router;
mod session;

/// The TLS 1.3 tickets sent per full handshake when something can redeem them (rustls' and OpenSSL's default).
const TLS13_TICKETS: usize = 2;

/// OpenSSL's default session timeout, which a profile without `session_timeout_seconds` has always had.
const DEFAULT_SESSION_TIMEOUT_SECONDS: u32 = 300;

impl TlsProfilePlan {
    /// Settings that hand the accepted connections of a listener to the rustls backend.
    ///
    /// # Errors
    ///
    /// Returns an error if the profile asks for the OpenSSL backend, a certificate identity cannot be used by rustls,
    /// or rustls rejects the profile's protocol or session settings.
    pub fn rustls_settings(&self, handshake_threads: usize) -> Result<CustomTlsSettings, TlsBuildError> {
        if self.backend != TlsBackend::Rustls {
            return Err(self.rustls_error("the profile selects the OpenSSL backend".into()));
        }
        for (name, active) in &self.selector.active_generations {
            if let Err(detail) = active.snapshot().certified_key() {
                return Err(self.rustls_error(format!("certificate `{name}`: {detail}")));
            }
        }
        let protocols = alpn_protocols(&self.alpn);
        let sessions = self.sessions()?;
        let strict = self.rustls_server_config(protocols.clone(), &sessions)?;
        // A client that offers protocols the profile does not speak is served without ALPN, as the OpenSSL backend
        // does, unless the profile speaks HTTP/2 only.
        let lenient = if matches!(self.alpn, ALPN::H2) {
            None
        } else {
            Some(self.rustls_server_config(Vec::new(), &sessions)?)
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

    fn rustls_server_config(
        &self,
        alpn_protocols: Vec<Vec<u8>>,
        sessions: &Sessions,
    ) -> Result<Arc<ServerConfig>, TlsBuildError> {
        let builder = ServerConfig::builder_with_provider(crate::tls::provider::aws_lc_provider())
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
    /// sent. Every configuration of the listener shares the one state, so a session resumes whichever serves it.
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

/// The resumption state the configurations of one listener share.
struct Sessions {
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
