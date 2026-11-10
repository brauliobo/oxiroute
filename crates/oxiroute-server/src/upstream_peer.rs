use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use http::{HeaderName, HeaderValue, Version};
use oxiroute_config::{HttpVersion, PoolResilience, UpstreamConnectionReuse};
use pingora::{Error, ErrorType, protocols::Digest, upstreams::peer::HttpPeer};
use tokio::time::Instant;

use crate::clock::promptly;
use crate::label::Label;
use crate::routing::{EndpointObservation, QueueTag};
use crate::{EndpointLease, H3UpstreamPlan, RoundRobinPool, RuntimeEndpoint, UpstreamTlsPlan};

/// Marks the 503 of a node that refused a request because none of its pool's servers can take it.
/// Such a request was never forwarded, so a cluster pool may safely redispatch it to the next node.
pub(crate) const POOL_UNAVAILABLE_HEADER: HeaderName =
    HeaderName::from_static("x-oxiroute-pool-unavailable");
pub(crate) const POOL_UNAVAILABLE: ErrorType = ErrorType::Custom("PoolUnavailable");

pub(crate) fn is_pool_unavailable(error: &Error) -> bool {
    error.etype() == &POOL_UNAVAILABLE
}

/// What the 503 of a pool without a selectable server carries beyond its marker header.
#[derive(Clone, Debug, Default)]
pub(crate) struct UnavailableResponse {
    pub(crate) retry_after: Option<HeaderValue>,
    pub(crate) body: Bytes,
}

impl UnavailableResponse {
    pub(crate) fn from_config(resilience: &PoolResilience) -> Self {
        Self {
            retry_after: resilience
                .unavailable_retry_after_seconds
                .map(HeaderValue::from),
            body: resilience
                .unavailable_body
                .as_deref()
                .map(|body| Bytes::copy_from_slice(body.as_bytes()))
                .unwrap_or_default(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct UpstreamPlan {
    connect_timeout: Option<Duration>,
    connection_reuse: UpstreamConnectionReuse,
    h3: Option<Arc<H3UpstreamPlan>>,
    http_version: HttpVersion,
    /// The pool's name, shared with the records that say where a request is.
    label: Label,
    selector: Arc<RoundRobinPool>,
    server_timeout: Option<Duration>,
    tls: Option<Arc<UpstreamTlsPlan>>,
    unavailable: UnavailableResponse,
}

static NEVER_REUSE_KEY: AtomicU64 = AtomicU64::new(1);

impl UpstreamPlan {
    #[cfg(test)]
    pub(crate) fn new(selector: Arc<RoundRobinPool>, tls: Option<Arc<UpstreamTlsPlan>>) -> Self {
        Self {
            connect_timeout: None,
            connection_reuse: UpstreamConnectionReuse::Safe,
            h3: None,
            http_version: HttpVersion::Http11,
            label: Label::new(selector.name()),
            selector,
            server_timeout: None,
            tls,
            unavailable: UnavailableResponse::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_policy(
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

    pub(crate) fn with_http_policy(
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
            label: Label::new(selector.name()),
            selector,
            server_timeout,
            tls,
            unavailable: UnavailableResponse::default(),
        }
    }

    #[must_use]
    pub(crate) fn with_unavailable_response(mut self, unavailable: UnavailableResponse) -> Self {
        self.unavailable = unavailable;
        self
    }

    pub(crate) const fn unavailable_response(&self) -> &UnavailableResponse {
        &self.unavailable
    }

    /// Whether requests reach the pool's servers as plain HTTP/1.1 over a stream the lane can drive itself, with
    /// nothing queued by class.
    pub(crate) fn is_plain_http1(&self) -> bool {
        self.tls.is_none()
            && self.h3.is_none()
            && self.http_version == HttpVersion::Http11
            && !self.selector.schedules_queue()
    }

    pub(crate) fn label(&self) -> &Label {
        &self.label
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
        tag: QueueTag,
    ) -> pingora::Result<SelectedEndpoint> {
        let lease = if self.http_version == HttpVersion::Http3
            || self.connection_reuse == UpstreamConnectionReuse::Never
        {
            self.selector.select_wait_excluding(excluded, tag).await
        } else {
            self.selector
                .select_connection_target_waiting(excluded, tag)
                .await
        };
        lease
            .map(SelectedEndpoint::new)
            .ok_or_else(|| Error::new_up(POOL_UNAVAILABLE))
    }

    pub(crate) async fn select_server_endpoint(
        &self,
        name: &str,
        tag: QueueTag,
    ) -> pingora::Result<SelectedEndpoint> {
        let lease = if self.http_version == HttpVersion::Http3
            || self.connection_reuse == UpstreamConnectionReuse::Never
        {
            self.selector.select_server_wait(name, tag).await
        } else {
            self.selector.select_server_connection_target(name)
        };
        lease
            .map(SelectedEndpoint::new)
            .ok_or_else(|| Error::new_up(POOL_UNAVAILABLE))
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
    unix_pending: bool,
}

impl SelectedEndpoint {
    fn new(lease: EndpointLease) -> Self {
        let observation = lease.observation();
        Self {
            addresses: None,
            deadline: None,
            endpoint: lease.endpoint().clone(),
            observation,
            lease,
            unix_pending: true,
        }
    }

    pub(crate) const fn endpoint(&self) -> &RuntimeEndpoint {
        &self.endpoint
    }

    pub(crate) fn server_name(&self) -> &str {
        self.lease.server_name()
    }

    pub(crate) fn observation(&self) -> EndpointObservation {
        self.observation.clone()
    }

    pub(crate) fn server_stats(
        &self,
    ) -> &std::sync::Arc<crate::observability::upstream_stats::ServerStats> {
        self.lease.server_stats()
    }

    pub(crate) fn end_queue_wait(&self) {
        self.lease.end_queue_wait();
    }

    pub(crate) fn queue_wait(&self) -> Duration {
        self.lease.queue_wait()
    }

    pub(crate) fn observe_queue_position(
        &self,
        cell: std::sync::Arc<std::sync::atomic::AtomicU64>,
    ) {
        self.lease.observe_queue_position(cell);
    }

    pub(crate) fn queue_position(&self) -> u64 {
        self.lease.queue_position()
    }

    #[cfg(test)]
    pub(crate) fn with_addresses(lease: EndpointLease, addresses: Vec<SocketAddr>) -> Self {
        let observation = lease.observation();
        Self {
            addresses: Some(addresses.into_iter()),
            deadline: None,
            endpoint: lease.endpoint().clone(),
            observation,
            lease,
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
        self.prepare_peer_with_timeouts(
            plan,
            std::time::Instant::now(),
            connection_timeout,
            io_timeout,
            io_timeout,
        )
        .await
    }

    /// `now` is the caller's reading of the clock for this step.
    pub(crate) async fn prepare_peer_with_timeouts(
        &mut self,
        plan: &UpstreamPlan,
        now: std::time::Instant,
        connection_timeout: Duration,
        read_timeout: Duration,
        write_timeout: Duration,
    ) -> pingora::Result<HttpPeer> {
        let mut remaining = self.remaining_timeout(Instant::from_std(now), connection_timeout)?;
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
            let (addresses, waited) = promptly(remaining, self.lease.resolve_addresses())
                .await
                .map_err(|_| endpoint_timeout(&self.endpoint, connection_timeout))?;
            self.addresses = Some(
                addresses
                    .map_err(|source| dns_failure(&self.endpoint, source))?
                    .into_iter(),
            );
            if waited {
                remaining = self.remaining_timeout(Instant::now(), connection_timeout)?;
            }
        }
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
            let remaining = self.remaining_timeout(Instant::now(), connection_timeout)?;
            let addresses = tokio::time::timeout(remaining, self.lease.resolve_addresses())
                .await
                .map_err(|_| endpoint_timeout(&self.endpoint, connection_timeout))?
                .map_err(|source| dns_failure(&self.endpoint, source))?;
            self.addresses = Some(addresses.into_iter());
        }
        let _remaining = self.remaining_timeout(Instant::now(), connection_timeout)?;
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

    fn remaining_timeout(&mut self, now: Instant, timeout: Duration) -> pingora::Result<Duration> {
        let deadline = *self.deadline.get_or_insert_with(|| now + timeout);
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
mod tests;
