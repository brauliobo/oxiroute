use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use http::Version;
use oxiroute_config::{HttpVersion, UpstreamConnectionReuse};
use pingora::{Error, ErrorType, protocols::Digest, upstreams::peer::HttpPeer};
use tokio::time::Instant;

use crate::routing::EndpointObservation;
use crate::{EndpointLease, H3UpstreamPlan, RoundRobinPool, RuntimeEndpoint, UpstreamTlsPlan};

#[derive(Debug)]
pub(crate) struct UpstreamPlan {
    connect_timeout: Option<Duration>,
    connection_reuse: UpstreamConnectionReuse,
    h3: Option<Arc<H3UpstreamPlan>>,
    http_version: HttpVersion,
    selector: Arc<RoundRobinPool>,
    server_timeout: Option<Duration>,
    tls: Option<Arc<UpstreamTlsPlan>>,
}

static NEVER_REUSE_KEY: AtomicU64 = AtomicU64::new(1);

impl UpstreamPlan {
    #[cfg(test)]
    pub(crate) const fn new(
        selector: Arc<RoundRobinPool>,
        tls: Option<Arc<UpstreamTlsPlan>>,
    ) -> Self {
        Self {
            connect_timeout: None,
            connection_reuse: UpstreamConnectionReuse::Safe,
            h3: None,
            http_version: HttpVersion::Http11,
            selector,
            server_timeout: None,
            tls,
        }
    }

    #[cfg(test)]
    pub(crate) const fn with_policy(
        selector: Arc<RoundRobinPool>,
        tls: Option<Arc<UpstreamTlsPlan>>,
        connect_timeout: Option<Duration>,
        server_timeout: Option<Duration>,
        connection_reuse: UpstreamConnectionReuse,
    ) -> Self {
        Self::with_http_policy(
            selector,
            tls,
            connect_timeout,
            server_timeout,
            connection_reuse,
            HttpVersion::Http11,
            None,
        )
    }

    pub(crate) const fn with_http_policy(
        selector: Arc<RoundRobinPool>,
        tls: Option<Arc<UpstreamTlsPlan>>,
        connect_timeout: Option<Duration>,
        server_timeout: Option<Duration>,
        connection_reuse: UpstreamConnectionReuse,
        http_version: HttpVersion,
        h3: Option<Arc<H3UpstreamPlan>>,
    ) -> Self {
        Self {
            connect_timeout,
            connection_reuse,
            h3,
            http_version,
            selector,
            server_timeout,
            tls,
        }
    }

    pub(crate) fn selector(&self) -> &Arc<RoundRobinPool> {
        &self.selector
    }

    pub(crate) fn tls(&self) -> Option<&UpstreamTlsPlan> {
        self.tls.as_deref()
    }

    pub(crate) fn h3(&self) -> Option<&H3UpstreamPlan> {
        self.h3.as_deref()
    }

    pub(crate) async fn select_endpoint(
        &self,
        excluded: &[String],
    ) -> pingora::Result<SelectedEndpoint> {
        let lease = if self.http_version == HttpVersion::Http3
            || self.connection_reuse == UpstreamConnectionReuse::Never
        {
            self.selector.select_wait_excluding(excluded).await
        } else {
            self.selector.select_connection_target_excluding(excluded)
        };
        lease
            .map(SelectedEndpoint::new)
            .ok_or_else(|| Error::new_up(ErrorType::HTTPStatus(503)))
    }

    pub(crate) async fn select_server_endpoint(
        &self,
        name: &str,
    ) -> pingora::Result<SelectedEndpoint> {
        let lease = if self.http_version == HttpVersion::Http3
            || self.connection_reuse == UpstreamConnectionReuse::Never
        {
            self.selector.select_server_wait(name).await
        } else {
            self.selector.select_server_connection_target(name)
        };
        lease
            .map(SelectedEndpoint::new)
            .ok_or_else(|| Error::new_up(ErrorType::HTTPStatus(503)))
    }

    pub(crate) fn has_unattempted(&self, attempted: &[String]) -> bool {
        self.selector.has_unattempted_servers(attempted)
    }

    pub(crate) const fn connection_reuse(&self) -> UpstreamConnectionReuse {
        self.connection_reuse
    }

    pub(crate) fn connect_timeout(&self, fallback: Duration) -> Duration {
        self.connect_timeout.unwrap_or(fallback)
    }

    pub(crate) fn server_timeout(&self, fallback: Duration) -> Duration {
        self.server_timeout.unwrap_or(fallback)
    }

    #[cfg(test)]
    fn peer(
        &self,
        address: SocketAddr,
        connection_timeout: Duration,
        io_timeout: Duration,
    ) -> HttpPeer {
        self.peer_with_timeouts(address, connection_timeout, io_timeout, io_timeout)
    }

    fn peer_with_timeouts(
        &self,
        address: SocketAddr,
        connection_timeout: Duration,
        read_timeout: Duration,
        write_timeout: Duration,
    ) -> HttpPeer {
        let mut peer = HttpPeer::new(address, false, String::new());
        self.configure_peer(&mut peer, connection_timeout, read_timeout, write_timeout);
        peer
    }

    fn unix_peer(
        &self,
        endpoint: &RuntimeEndpoint,
        connection_timeout: Duration,
        read_timeout: Duration,
        write_timeout: Duration,
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
        self.configure_peer(&mut peer, connection_timeout, read_timeout, write_timeout);
        Ok(peer)
    }

    fn configure_peer(
        &self,
        peer: &mut HttpPeer,
        connection_timeout: Duration,
        read_timeout: Duration,
        write_timeout: Duration,
    ) {
        if let Some(tls) = &self.tls {
            tls.apply_to_peer(peer);
        }
        if self.connection_reuse == UpstreamConnectionReuse::Never {
            peer.group_key = NEVER_REUSE_KEY.fetch_add(1, Ordering::Relaxed);
            peer.options.idle_timeout = Some(Duration::ZERO);
        }
        peer.options.connection_timeout = Some(connection_timeout);
        peer.options.total_connection_timeout = Some(connection_timeout);
        peer.options.read_timeout = Some(read_timeout);
        peer.options.write_timeout = Some(write_timeout);
    }
}

pub(crate) struct SelectedEndpoint {
    addresses: Option<std::vec::IntoIter<SocketAddr>>,
    deadline: Option<Instant>,
    endpoint: RuntimeEndpoint,
    observation: EndpointObservation,
    lease: EndpointLease,
    server_name: String,
    unix_pending: bool,
}

impl SelectedEndpoint {
    fn new(lease: EndpointLease) -> Self {
        let server_name = lease.server_name().to_owned();
        let observation = lease.observation();
        Self {
            addresses: None,
            deadline: None,
            endpoint: lease.endpoint().clone(),
            observation,
            lease,
            server_name,
            unix_pending: true,
        }
    }

    pub(crate) const fn endpoint(&self) -> &RuntimeEndpoint {
        &self.endpoint
    }

    pub(crate) fn server_name(&self) -> &str {
        &self.server_name
    }

    pub(crate) fn observation(&self) -> EndpointObservation {
        self.observation.clone()
    }

    #[cfg(test)]
    pub(crate) fn with_addresses(lease: EndpointLease, addresses: Vec<SocketAddr>) -> Self {
        let server_name = lease.server_name().to_owned();
        let observation = lease.observation();
        Self {
            addresses: Some(addresses.into_iter()),
            deadline: None,
            endpoint: lease.endpoint().clone(),
            observation,
            lease,
            server_name,
            unix_pending: true,
        }
    }

    #[cfg(test)]
    pub(crate) async fn prepare_peer(
        &mut self,
        plan: &UpstreamPlan,
        connection_timeout: Duration,
        io_timeout: Duration,
    ) -> pingora::Result<HttpPeer> {
        self.prepare_peer_with_timeouts(plan, connection_timeout, io_timeout, io_timeout)
            .await
    }

    pub(crate) async fn prepare_peer_with_timeouts(
        &mut self,
        plan: &UpstreamPlan,
        connection_timeout: Duration,
        read_timeout: Duration,
        write_timeout: Duration,
    ) -> pingora::Result<HttpPeer> {
        let remaining = self.remaining_timeout(connection_timeout)?;
        if matches!(self.endpoint, RuntimeEndpoint::Unix { .. }) {
            if !self.unix_pending {
                return Err(Error::new_in(ErrorType::InternalError));
            }
            self.unix_pending = false;
            let mut peer =
                plan.unix_peer(&self.endpoint, remaining, read_timeout, write_timeout)?;
            peer.connection_lifetime = Some(self.lease.connection_lifetime());
            return Ok(peer);
        }
        if self.addresses.is_none() {
            let addresses = tokio::time::timeout(remaining, self.lease.resolve_addresses())
                .await
                .map_err(|_| endpoint_timeout(&self.endpoint, connection_timeout))?
                .map_err(|source| dns_failure(&self.endpoint, source))?;
            self.addresses = Some(addresses.into_iter());
        }
        let remaining = self.remaining_timeout(connection_timeout)?;
        let address = self
            .addresses
            .as_mut()
            .and_then(Iterator::next)
            .ok_or_else(|| Error::new_in(ErrorType::InternalError))?;
        let mut peer = plan.peer_with_timeouts(address, remaining, read_timeout, write_timeout);
        peer.connection_lifetime = Some(self.lease.connection_lifetime());
        Ok(peer)
    }

    pub(crate) async fn prepare_h3_address(
        &mut self,
        connection_timeout: Duration,
    ) -> pingora::Result<SocketAddr> {
        if matches!(self.endpoint, RuntimeEndpoint::Unix { .. }) {
            return Err(Error::new_up(ErrorType::ConnectError));
        }
        if self.addresses.is_none() {
            let remaining = self.remaining_timeout(connection_timeout)?;
            let addresses = tokio::time::timeout(remaining, self.lease.resolve_addresses())
                .await
                .map_err(|_| endpoint_timeout(&self.endpoint, connection_timeout))?
                .map_err(|source| dns_failure(&self.endpoint, source))?;
            self.addresses = Some(addresses.into_iter());
        }
        let _remaining = self.remaining_timeout(connection_timeout)?;
        self.addresses
            .as_mut()
            .and_then(Iterator::next)
            .ok_or_else(|| Error::new_up(ErrorType::ConnectError))
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
            .prepare_peer(&plan, timeout, timeout)
            .await
            .expect("first peer");
        assert_eq!(first_peer.address().as_inet(), Some(&first));
        selected.deadline = Some(Instant::now());

        assert!(!selected.has_address_fallback());
        let error = selected
            .prepare_peer(&plan, timeout, timeout)
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
    fn pool_connect_server_timeouts_and_never_reuse_are_independent() {
        let address = SocketAddr::from(([127, 0, 0, 1], 8080));
        let selector = Arc::new(RoundRobinPool::new([address]).expect("selector"));
        let plan = UpstreamPlan::with_policy(
            selector,
            None,
            Some(Duration::from_secs(2)),
            Some(Duration::from_secs(11)),
            UpstreamConnectionReuse::Never,
        );

        let first = plan.peer(
            address,
            plan.connect_timeout(Duration::from_secs(30)),
            plan.server_timeout(Duration::from_secs(30)),
        );
        let second = plan.peer(
            address,
            plan.connect_timeout(Duration::from_secs(30)),
            plan.server_timeout(Duration::from_secs(30)),
        );
        assert_eq!(
            first.options.connection_timeout,
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            first.options.total_connection_timeout,
            Some(Duration::from_secs(2))
        );
        assert_eq!(first.options.read_timeout, Some(Duration::from_secs(11)));
        assert_eq!(first.options.write_timeout, Some(Duration::from_secs(11)));
        assert_eq!(first.options.idle_timeout, Some(Duration::ZERO));
        assert_ne!(first.reuse_hash(), second.reuse_hash());

        let always = UpstreamPlan::with_policy(
            Arc::new(RoundRobinPool::new([address]).expect("always selector")),
            None,
            None,
            None,
            UpstreamConnectionReuse::Always,
        );
        assert_eq!(
            always
                .peer(address, Duration::from_secs(1), Duration::from_secs(1))
                .reuse_hash(),
            always
                .peer(address, Duration::from_secs(1), Duration::from_secs(1))
                .reuse_hash()
        );
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
            servers: Vec::new(),
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
            queue_timeout_ms: None,
            connect_timeout_ms: None,
            server_timeout_ms: None,
            connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
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
