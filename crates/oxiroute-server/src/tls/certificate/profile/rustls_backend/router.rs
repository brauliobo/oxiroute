use std::{fmt, sync::Arc};

use oxiroute_tls_rustls::ConfigRouter;
use rustls::{
    ServerConfig, SupportedProtocolVersion,
    server::{ClientHello, ResolvesServerCert},
    sign::CertifiedKey,
};

use crate::tls::{
    TLS_ALPN_PROTOCOL, TlsAlpnChallengeStore, certificate::rustls_certified_key, provider::aws_lc_provider,
};

/// Picks the configuration of a connection from its ClientHello: the TLS-ALPN-01 challenge when the client asks for
/// it and a challenge is provisioned for its name, the profile's configuration otherwise.
pub(super) struct ProfileRouter {
    strict: Arc<ServerConfig>,
    lenient: Option<Arc<ServerConfig>>,
    protocols: Vec<Vec<u8>>,
    challenges: TlsAlpnChallengeStore,
    versions: &'static [&'static SupportedProtocolVersion],
}

impl ProfileRouter {
    pub(super) fn new(
        strict: Arc<ServerConfig>,
        lenient: Option<Arc<ServerConfig>>,
        protocols: Vec<Vec<u8>>,
        challenges: TlsAlpnChallengeStore,
        versions: &'static [&'static SupportedProtocolVersion],
    ) -> Self {
        Self {
            strict,
            lenient,
            protocols,
            challenges,
            versions,
        }
    }

    fn challenge_config(&self, hello: &ClientHello<'_>) -> Option<Arc<ServerConfig>> {
        let now = crate::tls::tls_alpn::unix_now();
        let identity = self.challenges.lookup(hello.server_name()?, now)?;
        if !identity.usable(now) {
            log::warn!(
                "TLS-ALPN-01 challenge identity expired or was cancelled for `{}`",
                identity.identifier()
            );
            return None;
        }
        let key = rustls_certified_key(identity.certificate(), &[], identity.private_key())
            .inspect_err(|error| {
                log::error!(
                    "failed to install TLS-ALPN-01 challenge identity for `{}`: {error}",
                    identity.identifier()
                );
            })
            .ok()?;
        let mut config = ServerConfig::builder_with_provider(aws_lc_provider())
            .with_protocol_versions(self.versions)
            .ok()?
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(ChallengeKey(Arc::new(key))));
        config.alpn_protocols = vec![TLS_ALPN_PROTOCOL.to_vec()];
        config.send_tls13_tickets = 0;
        Some(Arc::new(config))
    }
}

impl ConfigRouter for ProfileRouter {
    fn route(&self, hello: &ClientHello<'_>) -> Option<Arc<ServerConfig>> {
        let Some(offered) = hello.alpn() else {
            return Some(Arc::clone(&self.strict));
        };
        let (mut asks_challenge, mut overlaps, mut offers) = (false, false, false);
        for protocol in offered {
            offers = true;
            asks_challenge |= protocol == TLS_ALPN_PROTOCOL;
            overlaps |= self.protocols.iter().any(|spoken| spoken == protocol);
        }
        if asks_challenge {
            return self.challenge_config(hello);
        }
        if offers && !overlaps {
            return self.lenient.clone();
        }
        Some(Arc::clone(&self.strict))
    }
}

impl fmt::Debug for ProfileRouter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("ProfileRouter").finish_non_exhaustive()
    }
}

/// The one identity of a TLS-ALPN-01 challenge connection.
#[derive(Debug)]
struct ChallengeKey(Arc<CertifiedKey>);

impl ResolvesServerCert for ChallengeKey {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(Arc::clone(&self.0))
    }
}
