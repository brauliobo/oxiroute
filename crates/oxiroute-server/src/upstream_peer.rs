use std::{sync::Arc, time::Duration};

use http::Version;
use oxiroute_config::HttpVersion;
use pingora::{Error, ErrorType, protocols::Digest, upstreams::peer::HttpPeer};

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

    async fn peer(
        &self,
        endpoint: &RuntimeEndpoint,
        timeout: Duration,
    ) -> pingora::Result<HttpPeer> {
        let mut peer = match endpoint {
            RuntimeEndpoint::Socket { address } => HttpPeer::new(*address, false, String::new()),
            RuntimeEndpoint::Dns { .. } => {
                let addresses = tokio::time::timeout(timeout, endpoint.resolve())
                    .await
                    .map_err(|_| dns_timeout(endpoint, timeout))?
                    .map_err(|source| dns_failure(endpoint, source))?;
                HttpPeer::new(addresses[0], false, String::new())
            }
            RuntimeEndpoint::Unix { path } => HttpPeer::new_uds(
                path.to_str()
                    .expect("runtime Unix endpoints passed UTF-8 preflight"),
                false,
                String::new(),
            )?,
        };
        if let Some(tls) = &self.tls {
            tls.apply_to_peer(&mut peer);
        }
        peer.options.connection_timeout = Some(timeout);
        peer.options.total_connection_timeout = Some(timeout);
        peer.options.read_timeout = Some(timeout);
        peer.options.write_timeout = Some(timeout);
        Ok(peer)
    }
}

pub(crate) struct SelectedEndpoint {
    endpoint: RuntimeEndpoint,
    lease: EndpointLease,
}

impl SelectedEndpoint {
    fn new(lease: EndpointLease) -> Self {
        Self {
            endpoint: lease.endpoint().clone(),
            lease,
        }
    }

    pub(crate) const fn endpoint(&self) -> &RuntimeEndpoint {
        &self.endpoint
    }

    pub(crate) async fn prepare_peer(
        self,
        plan: &UpstreamPlan,
        timeout: Duration,
    ) -> pingora::Result<(HttpPeer, EndpointLease)> {
        let peer = plan.peer(&self.endpoint, timeout).await?;
        Ok((peer, self.lease))
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

fn dns_timeout(endpoint: &RuntimeEndpoint, timeout: Duration) -> Error {
    let mut error = *Error::explain(
        ErrorType::ConnectTimedout,
        format!("DNS resolution for `{endpoint}` timed out after {timeout:?}"),
    );
    error.as_up();
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
    async fn compiled_upstream_peer_retains_tls_policy_timeouts_and_reuse_isolation() {
        let timeout = Duration::from_secs(7);
        let address = SocketAddr::from(([127, 0, 0, 1], 443));
        let endpoint = RuntimeEndpoint::from(address);
        let tls = upstream_tls_plan(
            "origin.example.test",
            HttpVersion::Http11,
            HttpVersion::Http2,
        );
        let plan = UpstreamPlan::new(
            Arc::new(RoundRobinPool::new([address]).expect("selector")),
            Some(Arc::clone(&tls)),
        );

        let peer = plan.peer(&endpoint, timeout).await.expect("TLS peer");
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
            plan.peer(&endpoint, timeout)
                .await
                .expect("equivalent TLS peer")
                .reuse_hash()
        );

        let isolated_tls = upstream_tls_plan(
            "other.example.test",
            HttpVersion::Http11,
            HttpVersion::Http2,
        );
        let isolated = UpstreamPlan::new(Arc::clone(plan.selector()), Some(isolated_tls))
            .peer(&endpoint, timeout)
            .await
            .expect("isolated TLS peer");
        assert_ne!(peer.reuse_hash(), isolated.reuse_hash());

        let plaintext = UpstreamPlan::new(Arc::clone(plan.selector()), None)
            .peer(&endpoint, timeout)
            .await
            .expect("plaintext peer");
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
