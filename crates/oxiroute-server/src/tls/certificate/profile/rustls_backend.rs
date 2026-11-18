//! The rustls form of a TLS profile: one set of connection configurations per listener, a resolver that selects the
//! active certificate generation by SNI, and the router that picks between them from the `ClientHello`.

use std::sync::Arc;

use oxiroute_tls_rustls::{KernelTls, RustlsAcceptor};
use pingora::listeners::{CustomTlsSettings, tls::cipher_list_names};
use rustls::{
    ServerConfig, SupportedProtocolVersion,
    crypto::CryptoProvider,
    server::{NoServerSessionStorage, ProducesTickets, StoresServerSessions},
    version,
};

use self::{
    resolver::GenerationResolver,
    router::ProfileRouter,
    session::{SessionCache, TimedTicketer},
};
use super::{ALPN, KtlsMode, TLS13_TICKETS, TlsBackend, TlsBuildError, TlsProfilePlan, TlsVersion};
use crate::tls::{
    provider::{aws_lc_provider, with_cipher_list, with_key_exchange_groups},
    ticket_keys::TicketKeys,
};

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
        let router = self.rustls_router()?;
        let acceptor = RustlsAcceptor::new(Arc::new(router));
        let acceptor = match self.rustls_kernel {
            Some(kernel) => acceptor.with_kernel_tls(kernel),
            None => acceptor,
        };
        let mut settings = CustomTlsSettings::new(Arc::new(acceptor));
        if handshake_threads > 0 {
            settings.set_offload_threadpool(handshake_threads, 1);
        }
        Ok(settings)
    }

    /// Whether the epoll data plane can terminate TLS for this profile: only the rustls backend gives it a
    /// configuration to drive.
    #[must_use]
    pub fn serves_on_epoll(&self) -> bool {
        self.backend == TlsBackend::Rustls
    }

    /// The router the epoll data plane terminates this profile's connections with, sharing the configurations and
    /// resumption state of the profile's other listeners.
    ///
    /// # Errors
    ///
    /// Returns an error if the profile does not use the rustls backend or rustls rejects its settings.
    #[cfg(target_os = "linux")]
    pub fn epoll_router(&self) -> Result<Arc<dyn oxiroute_epoll_h1::TlsRouter>, TlsBuildError> {
        Ok(Arc::new(self.rustls_router()?))
    }

    fn rustls_router(&self) -> Result<ProfileRouter, TlsBuildError> {
        if self.backend != TlsBackend::Rustls {
            return Err(self.rustls_error("the profile selects the OpenSSL backend".into()));
        }
        for (name, active) in &self.selector.active_generations {
            if let Err(detail) = active.snapshot().certified_key() {
                return Err(self.rustls_error(format!("certificate `{name}`: {detail}")));
            }
        }
        let protocols = alpn_protocols(&self.alpn);
        let provider = self.rustls_provider()?;
        let sessions = self.shared_sessions();
        let strict = self.rustls_server_config(&provider, protocols.clone(), &sessions)?;
        // A client that offers protocols the profile does not speak is served without ALPN, as the OpenSSL backend
        // does, unless the profile speaks HTTP/2 only.
        let lenient = if matches!(self.alpn, ALPN::H2) {
            None
        } else {
            Some(self.rustls_server_config(&provider, Vec::new(), &sessions)?)
        };
        Ok(ProfileRouter::new(
            strict,
            lenient,
            protocols,
            self.tls_alpn_challenge_store.clone(),
            self.protocol_versions(),
        ))
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
            log::warn!("{}", left_out_message(&self.name, &left_out));
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
        config.enable_secret_extraction = self.rustls_kernel.is_some();
        Ok(Arc::new(config))
    }

    /// Resumption as the profile declares it: a named cache keeps sessions in memory (TLS 1.2 ids and TLS 1.3 stateful
    /// tickets), stateless tickets are encrypted by the profile's ticket keys, and a TLS 1.3 ticket nobody could redeem is not
    /// sent. Every configuration of the profile shares the one state, so a session resumes whichever serves it and
    /// whichever listener of the profile accepts the next connection. The next generation of the profile in the
    /// process shares the ticket keys and takes the cached sessions, so resumption survives a reload.
    fn shared_sessions(&self) -> Arc<Sessions> {
        Arc::clone(
            self.rustls_sessions
                .get_or_init(|| Arc::new(self.sessions())),
        )
    }

    fn sessions(&self) -> Sessions {
        let timeout = self
            .session_timeout
            .map_or(DEFAULT_SESSION_TIMEOUT_SECONDS, i32::unsigned_abs);
        let storage: Arc<dyn StoresServerSessions> = match &self.session_cache {
            Some(cache) => SessionCache::for_profile(
                &self.name,
                usize::try_from(cache.capacity).unwrap_or_default(),
                timeout,
            ),
            None => Arc::new(NoServerSessionStorage {}),
        };
        let tickets: Option<Arc<dyn ProducesTickets>> = if self.session_tickets {
            Some(Arc::new(TimedTicketer::new(
                TicketKeys::for_profile(&self.name),
                timeout,
            )))
        } else {
            None
        };
        let resumable = self.session_tickets || self.session_cache.is_some();
        Sessions {
            storage,
            tickets,
            tls13_tickets: if resumable { TLS13_TICKETS } else { 0 },
        }
    }

    fn rustls_error(&self, detail: String) -> TlsBuildError {
        TlsBuildError::RustlsSettings {
            profile: self.name.clone(),
            detail,
        }
    }
}

/// What the kernel offers a rustls profile for kernel TLS: the first large static file of a connection is sent with
/// `sendfile(2)` and the kernel encrypts the rest of it. A kernel that cannot do it leaves the profile in user space,
/// exactly as `ktls = "off"`, and the operator hears it once per generation, here.
pub(super) fn kernel_tls(profile: &str, mode: KtlsMode, backend: TlsBackend) -> Option<KernelTls> {
    if mode == KtlsMode::Off || backend != TlsBackend::Rustls {
        return None;
    }
    match oxiroute_tls_rustls::probe() {
        Ok(kernel) => {
            log::info!("TLS profile `{profile}`: kernel TLS is active for large static files");
            Some(kernel)
        }
        Err(unavailable) => {
            log::info!(
                "TLS profile `{profile}`: kernel TLS is not available, TLS stays in user space: {unavailable}"
            );
            None
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

fn left_out_message(profile: &str, left_out: &[String]) -> String {
    let shown = left_out
        .iter()
        .take(6)
        .map(String::as_str)
        .collect::<Vec<_>>();
    let more = if left_out.len() > shown.len() {
        ", ..."
    } else {
        ""
    };
    format!(
        "TLS profile `{profile}`: policy.cipher_list selects {} TLS 1.2 suites the rustls backend does not implement and does not offer: {}{more}. Clients that need them fail the handshake. To serve them, move those clients to their own listener and TLS profile with policy.backend = \"openssl\" and an explicit policy.cipher_list (see \"Serving legacy clients\" in the operations guide), or set policy.backend = \"openssl\" on this profile",
        left_out.len(),
        shown.join(", "),
    )
}

#[cfg(test)]
mod tests {
    use super::left_out_message;

    #[test]
    fn the_left_out_message_names_the_setting_the_suites_and_the_remedy() {
        let left_out = ["DHE-RSA-AES128-GCM-SHA256", "AES128-SHA"].map(String::from);

        let message = left_out_message("public", &left_out);

        assert!(message.contains("`public`"), "{message}");
        assert!(message.contains("policy.cipher_list"), "{message}");
        assert!(
            message.contains("DHE-RSA-AES128-GCM-SHA256, AES128-SHA"),
            "{message}"
        );
        assert!(
            message.contains("policy.backend = \"openssl\""),
            "{message}"
        );
        assert!(message.contains("Serving legacy clients"), "{message}");
    }

    #[test]
    fn a_long_left_out_list_is_cut_after_six_names() {
        let left_out = (0..8)
            .map(|index| format!("SUITE-{index}"))
            .collect::<Vec<_>>();

        let message = left_out_message("public", &left_out);

        assert!(message.contains("SUITE-5, ..."), "{message}");
        assert!(!message.contains("SUITE-6"), "{message}");
    }
}
