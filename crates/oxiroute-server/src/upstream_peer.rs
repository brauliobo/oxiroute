use std::{net::SocketAddr, sync::Arc, time::Duration};

use http::Version;
use oxiroute_config::HttpVersion;
use pingora::{Error, ErrorType, protocols::Digest, upstreams::peer::HttpPeer};
use tokio::time::Instant;

use crate::{EndpointLease, RoundRobinPool, RuntimeEndpoint, UpstreamTlsPlan};

#[derive(Debug)]
pub(crate) struct UpstreamPlan {
    selector: Arc<RoundRobinPool>,
    tls: Option<Arc<UpstreamTlsPlan>>,
}

impl UpstreamPlan {
    pub(crate) const fn new(
        selector: Arc<RoundRobinPool>,
        tls: Option<Arc<UpstreamTlsPlan>>,
    ) -> Self {
        Self { selector, tls }
    }

    pub(crate) fn selector(&self) -> &Arc<RoundRobinPool> {
        &self.selector
    }

    pub(crate) fn tls(&self) -> Option<&UpstreamTlsPlan> {
        self.tls.as_deref()
    }

    pub(crate) fn select_endpoint(
        &self,
        excluded: &[RuntimeEndpoint],
    ) -> pingora::Result<SelectedEndpoint> {
        self.selector
            .select_excluding(excluded)
            .map(SelectedEndpoint::new)
            .ok_or_else(|| Error::new_up(ErrorType::HTTPStatus(503)))
    }

    pub(crate) fn has_available_endpoint(&self) -> bool {
        if self.selector.has_available() {
            true
        } else {
            self.selector.note_unavailable_selection();
            false
        }
    }

    pub(crate) fn has_unattempted(&self, attempted: &[RuntimeEndpoint]) -> bool {
        self.selector.has_unattempted(attempted)
    }

    fn peer(
        &self,
        address: SocketAddr,
        connection_timeout: Duration,
        io_timeout: Duration,
    ) -> HttpPeer {
        let mut peer = HttpPeer::new(address, false, String::new());
        self.configure_peer(&mut peer, connection_timeout, io_timeout);
        peer
    }

    fn unix_peer(
        &self,
        endpoint: &RuntimeEndpoint,
        connection_timeout: Duration,
        io_timeout: Duration,
    ) -> pingora::Result<HttpPeer> {
        let RuntimeEndpoint::Unix { path } = endpoint else {
            return Err(Error::new_in(ErrorType::InternalError));
        };
        let mut peer = HttpPeer::new_uds(
            path.to_str()
                .expect("runtime Unix endpoints passed UTF-8 preflight"),
            false,
            String::new(),
        )?;
        self.configure_peer(&mut peer, connection_timeout, io_timeout);
        Ok(peer)
    }

    fn configure_peer(
        &self,
        peer: &mut HttpPeer,
        connection_timeout: Duration,
        io_timeout: Duration,
    ) {
        if let Some(tls) = &self.tls {
            tls.apply_to_peer(peer);
        }
        peer.options.connection_timeout = Some(connection_timeout);
        peer.options.total_connection_timeout = Some(connection_timeout);
        peer.options.read_timeout = Some(io_timeout);
        peer.options.write_timeout = Some(io_timeout);
    }
}

pub(crate) struct SelectedEndpoint {
    addresses: Option<std::vec::IntoIter<SocketAddr>>,
    deadline: Option<Instant>,
    endpoint: RuntimeEndpoint,
    _lease: EndpointLease,
    unix_pending: bool,
}

impl SelectedEndpoint {
    fn new(lease: EndpointLease) -> Self {
        Self {
            addresses: None,
            deadline: None,
            endpoint: lease.endpoint().clone(),
            _lease: lease,
            unix_pending: true,
        }
    }

    pub(crate) const fn endpoint(&self) -> &RuntimeEndpoint {
        &self.endpoint
    }

    #[cfg(test)]
    pub(crate) fn with_addresses(lease: EndpointLease, addresses: Vec<SocketAddr>) -> Self {
        Self {
            addresses: Some(addresses.into_iter()),
            deadline: None,
            endpoint: lease.endpoint().clone(),
            _lease: lease,
            unix_pending: true,
        }
    }

    pub(crate) async fn prepare_peer(
        &mut self,
        plan: &UpstreamPlan,
        timeout: Duration,
    ) -> pingora::Result<HttpPeer> {
        let remaining = self.remaining_timeout(timeout)?;
        if matches!(self.endpoint, RuntimeEndpoint::Unix { .. }) {
            if !self.unix_pending {
                return Err(Error::new_in(ErrorType::InternalError));
            }
            self.unix_pending = false;
            return plan.unix_peer(&self.endpoint, remaining, timeout);
        }
        if self.addresses.is_none() {
            let addresses = tokio::time::timeout(remaining, self.endpoint.resolve_addresses())
                .await
                .map_err(|_| endpoint_timeout(&self.endpoint, timeout))?
                .map_err(|source| dns_failure(&self.endpoint, source))?;
            self.addresses = Some(addresses.into_iter());
        }
        let remaining = self.remaining_timeout(timeout)?;
        let address = self
            .addresses
            .as_mut()
            .and_then(Iterator::next)
            .ok_or_else(|| Error::new_in(ErrorType::InternalError))?;
        Ok(plan.peer(address, remaining, timeout))
    }

    pub(crate) fn has_address_fallback(&self) -> bool {
        self.deadline
            .is_none_or(|deadline| Instant::now() < deadline)
            && self
                .addresses
                .as_ref()
                .is_some_and(|addresses| !addresses.as_slice().is_empty())
    }

    fn remaining_timeout(&mut self, timeout: Duration) -> pingora::Result<Duration> {
        let deadline = *self
            .deadline
            .get_or_insert_with(|| Instant::now() + timeout);
        let now = Instant::now();
        if now >= deadline {
            Err(endpoint_timeout(&self.endpoint, timeout).into())
        } else {
            Ok(deadline - now)
        }
    }
}

pub(crate) fn enforce_http_version(
    tls: Option<&UpstreamTlsPlan>,
    actual: Version,
) -> pingora::Result<()> {
    if tls.is_some_and(|tls| tls.min_http_version() == HttpVersion::Http2)
        && actual != Version::HTTP_2
    {
        return reject_policy(
            ErrorType::H2Downgrade,
            format!(
                "upstream policy requires HTTP/2, but the negotiated request version is {actual:?}"
            ),
        );
    }
    Ok(())
}

pub(crate) fn validate_tls_connection(
    tls: Option<&UpstreamTlsPlan>,
    digest: Option<&Digest>,
) -> pingora::Result<()> {
    let Some(tls) = tls else {
        return Ok(());
    };
    let Some(ssl) = digest.and_then(|digest| digest.ssl_digest.as_deref()) else {
        return reject_policy(
            ErrorType::Custom("UpstreamTlsPolicy"),
            format!(
                "TLS upstream `{}` has no negotiated TLS digest",
                tls.server_name()
            ),
        );
    };
    if !matches!(ssl.version.as_ref(), "TLSv1.2" | "TLSv1.3") {
        return reject_policy(
            ErrorType::Custom("UpstreamTlsPolicy"),
            format!(
                "TLS upstream `{}` negotiated unsupported protocol `{}`; TLSv1.2 or TLSv1.3 is required",
                tls.server_name(),
                ssl.version
            ),
        );
    }
    Ok(())
}

fn endpoint_timeout(endpoint: &RuntimeEndpoint, timeout: Duration) -> Error {
    let mut error = *Error::explain(
        ErrorType::ConnectTimedout,
        format!("connection attempts for `{endpoint}` timed out after {timeout:?}"),
    );
    error.as_up();
    error.set_retry(false);
    error
}

fn dns_failure(endpoint: &RuntimeEndpoint, source: std::io::Error) -> Error {
    let mut error = *Error::because(
        ErrorType::ConnectError,
        format!("DNS resolution for `{endpoint}` failed"),
        source,
    );
    error.as_up();
    error
}

fn reject_policy(error_type: ErrorType, context: String) -> pingora::Result<()> {
    let mut error = Error::explain(error_type, context).into_up();
    error.set_retry(false);
    Err(error)
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, sync::Arc, time::Duration};

    use http::Version;
    use oxiroute_config::{
        HttpVersion, HttpVersionPolicy, UpstreamAlgorithm, UpstreamEndpoint, UpstreamPool,
        UpstreamTls,
    };
    use pingora::{
        ErrorSource,
        listeners::ALPN,
        protocols::{Digest, tls::SslDigest},
        upstreams::peer::Peer,
    };

    use super::*;

    #[tokio::test]
    async fn endpoint_deadline_stops_address_fallback_with_a_non_retryable_timeout() {
        let first = SocketAddr::from(([192, 0, 2, 1], 443));
        let second = SocketAddr::from(([192, 0, 2, 2], 443));
        let endpoint = RuntimeEndpoint::Dns {
            host: "origin.example.test".into(),
            port: 443,
        };
        let selector = Arc::new(
            RoundRobinPool::new_named(
                "deadline".into(),
                [endpoint],
                UpstreamAlgorithm::RoundRobin,
                false,
            )
            .expect("deadline selector"),
        );
        let mut selected = SelectedEndpoint::with_addresses(
            selector.select().expect("deadline lease"),
            vec![first, second],
        );
        let plan = UpstreamPlan::new(selector, None);
        let timeout = Duration::from_secs(1);

        let first_peer = selected
            .prepare_peer(&plan, timeout)
            .await
            .expect("first peer");
        assert_eq!(first_peer.address().as_inet(), Some(&first));
        selected.deadline = Some(Instant::now());

        assert!(!selected.has_address_fallback());
        let error = selected
            .prepare_peer(&plan, timeout)
            .await
            .expect_err("expired deadline must stop fallback");
        assert_eq!(error.etype(), &ErrorType::ConnectTimedout);
        assert_eq!(error.esource(), &ErrorSource::Upstream);
        assert!(!error.retry());
        assert_eq!(
            selected
                .addresses
                .as_ref()
                .expect("resolved addresses")
                .as_slice(),
            &[second]
        );
    }

    #[tokio::test]
    async fn compiled_upstream_peer_retains_tls_policy_timeouts_and_reuse_isolation() {
        let timeout = Duration::from_secs(7);
        let address = SocketAddr::from(([127, 0, 0, 1], 443));
        let tls = upstream_tls_plan(
            "origin.example.test",
            HttpVersion::Http11,
            HttpVersion::Http2,
        );
        let plan = UpstreamPlan::new(
            Arc::new(RoundRobinPool::new([address]).expect("selector")),
            Some(Arc::clone(&tls)),
        );

        let peer = plan.peer(address, timeout, timeout);
        assert!(peer.is_tls());
        assert_eq!(peer.sni, "origin.example.test");
        assert!(peer.options.verify_cert);
        assert!(peer.options.verify_hostname);
        assert_eq!(peer.options.alpn, ALPN::H2H1);
        assert_eq!(peer.group_key, tls.group_key());
        assert_eq!(peer.options.connection_timeout, Some(timeout));
        assert_eq!(peer.options.total_connection_timeout, Some(timeout));
        assert_eq!(peer.options.read_timeout, Some(timeout));
        assert_eq!(peer.options.write_timeout, Some(timeout));
        assert_eq!(
            peer.reuse_hash(),
            plan.peer(address, timeout, timeout).reuse_hash()
        );

        let isolated_tls = upstream_tls_plan(
            "other.example.test",
            HttpVersion::Http11,
            HttpVersion::Http2,
        );
        let isolated = UpstreamPlan::new(Arc::clone(plan.selector()), Some(isolated_tls))
            .peer(address, timeout, timeout);
        assert_ne!(peer.reuse_hash(), isolated.reuse_hash());

        let plaintext =
            UpstreamPlan::new(Arc::clone(plan.selector()), None).peer(address, timeout, timeout);
        assert!(!plaintext.is_tls());
        assert!(plaintext.sni.is_empty());
        assert_eq!(plaintext.group_key, 0);
        assert_eq!(plaintext.options.alpn, ALPN::H1);
        assert_eq!(plaintext.options.connection_timeout, Some(timeout));
        assert_eq!(plaintext.options.total_connection_timeout, Some(timeout));
        assert_eq!(plaintext.options.read_timeout, Some(timeout));
        assert_eq!(plaintext.options.write_timeout, Some(timeout));
    }

    #[test]
    fn h2_only_policy_rejects_downgrade_before_upstream_write() {
        let tls = upstream_tls_plan(
            "origin.example.test",
            HttpVersion::Http2,
            HttpVersion::Http2,
        );

        let error = enforce_http_version(Some(&tls), Version::HTTP_11)
            .expect_err("HTTP/1.1 downgrade must fail");
        assert_eq!(error.etype(), &ErrorType::H2Downgrade);
        assert_eq!(error.esource(), &ErrorSource::Upstream);
        assert!(!error.retry());
        assert!(error.to_string().contains("requires HTTP/2"));
        assert!(enforce_http_version(Some(&tls), Version::HTTP_2).is_ok());
    }

    #[test]
    fn tls_version_policy_requires_a_modern_negotiated_digest() {
        let tls = upstream_tls_plan(
            "origin.example.test",
            HttpVersion::Http11,
            HttpVersion::Http11,
        );

        for digest in [None, Some(&Digest::default())] {
            let error = validate_tls_connection(Some(&tls), digest)
                .expect_err("missing TLS digest must fail");
            assert_eq!(error.etype(), &ErrorType::Custom("UpstreamTlsPolicy"));
            assert_eq!(error.esource(), &ErrorSource::Upstream);
            assert!(!error.retry());
        }
        for version in ["TLSv1", "TLSv1.1", "unknown"] {
            let digest = tls_digest(version);
            let error = validate_tls_connection(Some(&tls), Some(&digest))
                .expect_err("obsolete or unknown TLS must fail");
            assert!(error.to_string().contains(version));
            assert!(!error.retry());
        }
        for version in ["TLSv1.2", "TLSv1.3"] {
            let digest = tls_digest(version);
            assert!(validate_tls_connection(Some(&tls), Some(&digest)).is_ok());
        }
        assert!(validate_tls_connection(None, None).is_ok());
    }

    fn upstream_tls_plan(
        server_name: &str,
        min: HttpVersion,
        max: HttpVersion,
    ) -> Arc<UpstreamTlsPlan> {
        let pool = UpstreamPool {
            name: "origin".into(),
            endpoints: vec![UpstreamEndpoint::Socket {
                address: SocketAddr::from(([127, 0, 0, 1], 443)),
            }],
            algorithm: UpstreamAlgorithm::RoundRobin,
            health_check: None,
            tls: Some(UpstreamTls {
                server_name: server_name.into(),
                ca_certificate_path: None,
            }),
            http_versions: HttpVersionPolicy { min, max },
        };
        Arc::new(
            crate::tls::prepare_upstream_tls(&pool)
                .expect("upstream TLS plan")
                .expect("TLS enabled"),
        )
    }

    fn tls_digest(version: &'static str) -> Digest {
        Digest {
            ssl_digest: Some(Arc::new(SslDigest::new(
                "test cipher",
                version,
                None,
                None,
                Vec::new(),
            ))),
            ..Digest::default()
        }
    }
}
