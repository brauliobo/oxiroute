use std::{io, sync::Arc};

use log::warn;
use oxiroute_epoll_h1::Handoff;
use oxiroute_tls_rustls::TlsStream;
use pingora::{
    apps::{ConnectionAdmission, ServerApp},
    protocols::{Stream, l4::stream::Stream as Socket},
    server::ShutdownWatch,
    services::listening::Service,
};
use tokio::sync::mpsc::UnboundedReceiver;

fn adopt(handoff: Handoff<ConnectionAdmission>) -> io::Result<Stream> {
    let Handoff {
        stream,
        tls,
        prefix,
        peer,
        admission,
        ..
    } = handoff;
    let Some(conn) = tls else {
        let mut socket = Socket::adopt(stream, peer, &prefix)?;
        socket.hold_admission(admission);
        return Ok(Box::new(socket));
    };
    let mut socket = Socket::adopt(stream, peer, &[])?;
    socket.hold_admission(admission);
    Ok(Box::new(TlsStream::resume(socket, *conn, prefix)))
}

pub(super) async fn serve<A: ServerApp + Send + Sync + 'static>(
    mut handoffs: UnboundedReceiver<Handoff<ConnectionAdmission>>,
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
