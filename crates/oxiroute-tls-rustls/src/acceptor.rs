use std::{io, sync::Arc, task::Poll, time::Instant};

use async_trait::async_trait;
use pingora::{
    ErrorType::TLSHandshakeFailure,
    OrErr, Result,
    listeners::StreamTlsAcceptor,
    protocols::{Stream, l4::stream::Stream as L4Stream},
};
use rustls::{
    ServerConfig,
    server::{Accepted, AcceptedAlert, Acceptor, ClientHello},
};
use std::future::poll_fn;
use tokio::io::AsyncWriteExt;

use crate::{
    pump::{Pump, Transport},
    stream::TlsStream,
};

/// A fatal `no_application_protocol` alert record, sent when the router refuses a ClientHello.
const NO_APPLICATION_PROTOCOL_ALERT: [u8; 7] = [0x15, 0x03, 0x03, 0x00, 0x02, 0x02, 0x78];

/// Chooses the configuration that serves a connection once its ClientHello is known.
pub trait ConfigRouter: Send + Sync + 'static {
    /// `None` refuses the connection with a fatal `no_application_protocol` alert.
    fn route(&self, hello: &ClientHello<'_>) -> Option<Arc<ServerConfig>>;
}

/// Runs the server handshake of accepted connections with the configuration a [`ConfigRouter`] picks.
pub struct RustlsAcceptor {
    router: Arc<dyn ConfigRouter>,
}

impl RustlsAcceptor {
    pub fn new(router: Arc<dyn ConfigRouter>) -> Self {
        Self { router }
    }
}

#[async_trait]
impl StreamTlsAcceptor for RustlsAcceptor {
    async fn accept(&self, mut io: L4Stream) -> Result<Stream> {
        let started = Instant::now();
        let accepted = read_client_hello(&mut io)
            .await
            .explain_err(TLSHandshakeFailure, |error| format!("TLS accept() failed: {error}"))?;
        let Some(config) = self.router.route(&accepted.client_hello()) else {
            // Best effort: the connection is refused either way.
            let _ = io.write_all(&NO_APPLICATION_PROTOCOL_ALERT).await;
            let _ = io.flush().await;
            return Err(pingora::Error::explain(
                TLSHandshakeFailure,
                "TLS accept() failed: no application protocol could be agreed",
            ));
        };
        let conn = match accepted.into_connection(config) {
            Ok(conn) => conn,
            Err((error, alert)) => {
                send_alert(&mut io, alert).await;
                return Err(pingora::Error::explain(
                    TLSHandshakeFailure,
                    format!("TLS accept() failed: {error}"),
                ));
            }
        };
        let mut pump = Pump::new(io, conn);
        poll_fn(|context| pump.poll_handshake(context))
            .await
            .explain_err(TLSHandshakeFailure, |error| format!("TLS accept() failed: {error}"))?;
        Ok(Box::new(TlsStream::established(pump, started)))
    }
}

/// Reads until the ClientHello is complete.
async fn read_client_hello(io: &mut L4Stream) -> io::Result<Accepted> {
    let mut acceptor = Acceptor::default();
    loop {
        let read = poll_fn(|context| {
            let read = acceptor.read_tls(&mut Transport { io: &mut *io, context });
            match read {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => Poll::Pending,
                read => Poll::Ready(read),
            }
        })
        .await?;
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        match acceptor.accept() {
            Ok(Some(accepted)) => return Ok(accepted),
            Ok(None) => {}
            Err((error, alert)) => {
                send_alert(io, alert).await;
                return Err(io::Error::new(io::ErrorKind::InvalidData, error));
            }
        }
    }
}

/// Best effort: the connection is failing already and the peer only gains from learning why.
async fn send_alert(io: &mut L4Stream, mut alert: AcceptedAlert) {
    let written = poll_fn(|context| {
        loop {
            match alert.write(&mut Transport { io: &mut *io, context }) {
                Ok(0) => return Poll::Ready(Ok(())),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Poll::Pending,
                Err(error) => return Poll::Ready(Err(error)),
            }
        }
    })
    .await;
    if written.is_ok() {
        let _ = io.flush().await;
    }
}
