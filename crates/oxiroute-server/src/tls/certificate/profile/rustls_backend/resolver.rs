use std::{fmt, sync::Arc};

use rustls::{server::ClientHello, server::ResolvesServerCert, sign::CertifiedKey};

use crate::tls::certificate::CertificateSelector;

/// Selects the active generation of the certificate the ClientHello's SNI names, so a published replacement serves the
/// next handshake without touching the connection configuration.
pub(super) struct GenerationResolver {
    profile: String,
    selector: Arc<CertificateSelector>,
}

impl GenerationResolver {
    pub(super) fn new(profile: String, selector: Arc<CertificateSelector>) -> Self {
        Self { profile, selector }
    }
}

impl fmt::Debug for GenerationResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GenerationResolver")
            .field("profile", &self.profile)
            .finish_non_exhaustive()
    }
}

impl ResolvesServerCert for GenerationResolver {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let generation = self.selector.select(hello.server_name()).snapshot();
        match generation.certified_key() {
            Ok(key) => Some(Arc::clone(key)),
            Err(error) => {
                log::error!(
                    "failed to install certificate generation for TLS profile `{}` revision {}: {error}",
                    self.profile,
                    generation.metadata.revision
                );
                None
            }
        }
    }
}
