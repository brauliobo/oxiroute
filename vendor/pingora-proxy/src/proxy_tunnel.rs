//! An upgraded HTTP/1 connection (a WebSocket, say) after its handshake, relayed outside the request's state machine.
//!
//! The future of a request holds every hook and filter the request can pass through, and an upgraded connection can
//! live for hours. When the handshake is complete the request future ends (its heap block goes back to the thread's
//! pool and serves the next request), and the connection runs [`HttpProxy::run_tunnel`] instead: the two sessions, the
//! request context and a loop that waits for the next bytes of either side. The hooks and the end of the request
//! (logging) run as they did, only from a smaller future.

use super::*;
use crate::proxy_h1_inline::Exchange;

/// What an upgraded exchange carries from the request's state machine to the tunnel. The parts are set by the layers
/// of the request that own them as the request unwinds.
pub(crate) struct TunnelState {
    exchange: Exchange,
    client: Option<Box<HttpSessionV1>>,
    client_reused: bool,
    // A cancellation future is `Send` only and a `Session` is shared by reference across awaits.
    cancelled: std::sync::Mutex<Option<UpstreamCancellation>>,
    peer: Option<Box<HttpPeer>>,
    initial_write_pending: Duration,
}

impl TunnelState {
    pub(crate) fn new(exchange: Exchange) -> Self {
        Self {
            exchange,
            client: None,
            client_reused: false,
            cancelled: Default::default(),
            peer: None,
            initial_write_pending: Duration::ZERO,
        }
    }

    pub(crate) fn upstream(&mut self, client: Box<HttpSessionV1>, reused: bool) {
        self.client = Some(client);
        self.client_reused = reused;
    }

    pub(crate) fn continue_with(&mut self, cancelled: UpstreamCancellation, peer: Box<HttpPeer>) {
        *self.cancelled.get_mut().expect("unshared") = Some(cancelled);
        self.peer = Some(peer);
    }

    pub(crate) fn write_pending_before(&mut self, pending: Duration) {
        self.initial_write_pending = pending;
    }

    #[cfg(test)]
    pub(crate) fn into_exchange(self) -> Exchange {
        self.exchange
    }
}

impl<SV, C> HttpProxy<SV, C>
where
    C: custom::Connector,
{
    /// Relays an upgraded connection to its end, then ends the request as a request ends: the response to an error,
    /// the logging hook.
    pub(crate) async fn run_tunnel(
        self: Arc<Self>,
        mut session: Box<Session>,
        mut ctx: Box<<SV as ProxyHttp>::CTX>,
    ) where
        SV: ProxyHttp + Send + Sync + 'static,
        <SV as ProxyHttp>::CTX: Send + Sync,
    {
        let tunnel = session.tunnel.take().expect("a tunnel carries its state");
        let TunnelState {
            exchange,
            client,
            client_reused,
            cancelled,
            peer,
            initial_write_pending,
        } = *tunnel;
        let (mut client, mut cancelled, peer) = (
            client.expect("a tunnel has its upstream session"),
            cancelled
                .into_inner()
                .expect("unshared")
                .expect("a tunnel has the watch of its cancellation"),
            peer.expect("a tunnel has its peer"),
        );
        let (server_reuse, error) = tokio::select! {
            biased;
            error = &mut cancelled => {
                (false, Some(self.inner.error_while_proxy(&peer, &mut session, error, &mut ctx, false)))
            }
            (reuse_downstream, _, error) = self.relay_resumed(&mut session, &mut client, exchange, &mut ctx) => {
                (
                    reuse_downstream,
                    error.map(|e| self.inner.error_while_proxy(&peer, &mut session, e, &mut ctx, client_reused)),
                )
            }
        };
        session.set_upstream_body_bytes_received(client.body_bytes_received());
        session.set_upstream_body_bytes_sent(client.body_bytes_sent());
        let write_pending = client.stream().get_write_pending_time();
        session
            .set_upstream_write_pending_time(write_pending.saturating_sub(initial_write_pending));
        // The upstream connection of an upgrade is not reused.
        drop(client);
        // On the heap: the end of a request is 5 KiB of hooks, needed once, after the tunnel.
        Box::pin(self.conclude(*session, &mut ctx, server_reuse, error, 1)).await;
    }
}
