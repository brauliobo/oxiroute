use std::{io, sync::Arc};

use log::warn;
use oxiroute_epoll_h1::Handoff;
use oxiroute_tls_rustls::TlsStream;
use pingora::{
    apps::ServerApp,
    protocols::{GetSocketDigest as _, Stream, l4::stream::Stream as Socket},
    server::ShutdownWatch,
    services::listening::Service,
};
use tokio::sync::mpsc::UnboundedReceiver;

use super::admission::EngineAdmission;

fn adopt(handoff: Handoff<EngineAdmission>) -> io::Result<Stream> {
    let Handoff {
        stream,
        tls,
        prefix,
        peer,
        admission,
        ..
    } = handoff;
    let EngineAdmission { held, tracked } = admission;
    let Some(conn) = tls else {
        let mut socket = Socket::adopt(stream, peer, &prefix)?;
        socket.hold_admission(held);
        track(&socket, tracked);
        return Ok(Box::new(socket));
    };
    let mut socket = Socket::adopt(stream, peer, &[])?;
    socket.hold_admission(held);
    track(&socket, tracked);
    Ok(Box::new(TlsStream::resume(socket, *conn, prefix)))
}

fn track(socket: &Socket, tracked: Option<crate::clients::Connection>) {
    if let (Some(digest), Some(tracked)) = (socket.get_socket_digest(), tracked) {
        let _ = digest.tracking_or_init(|| Some(std::sync::Arc::new(tracked)));
    }
}

pub(super) async fn serve<A: ServerApp + Send + Sync + 'static>(
    mut handoffs: UnboundedReceiver<Handoff<EngineAdmission>>,
    app: Arc<A>,
    shutdown: ShutdownWatch,
) {
    while let Some(handoff) = handoffs.recv().await {
        match adopt(handoff) {
            Ok(stream) => {
                tokio::spawn(Service::handle_event(
                    stream,
                    Arc::clone(&app),
                    shutdown.clone(),
                ));
            }
            Err(error) => warn!("could not hand a connection to the tokio data plane: {error}"),
        }
    }
}
