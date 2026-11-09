//! A request that reaches an HTTP/2 connection while its service is shutting down.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use pingora_http::ResponseHeader;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::time::timeout;

use super::{HttpServerApp, HttpServerOptions, ReusedHttpStream, ServerApp};
use crate::protocols::http::ServerSession;
use crate::protocols::l4::stream::Stream as L4Stream;
use crate::server::ShutdownWatch;

struct Answering {
    options: HttpServerOptions,
}

#[async_trait]
impl HttpServerApp for Answering {
    async fn process_new_http(
        self: &Arc<Self>,
        mut session: ServerSession,
        _shutdown: &ShutdownWatch,
    ) -> Option<ReusedHttpStream> {
        let mut header = ResponseHeader::build(200, None).unwrap();
        header.insert_header("content-length", "2").unwrap();
        session
            .write_response_header(Box::new(header))
            .await
            .unwrap();
        session
            .write_response_body(Bytes::from_static(b"ok"), true)
            .await
            .unwrap();
        session.finish().await.unwrap();
        None
    }

    fn server_options(&self) -> Option<&HttpServerOptions> {
        Some(&self.options)
    }
}

/// The request is on the wire before the server reads its connection, and so is the signal to shut down: when the
/// server starts to shut the connection down, the request is already waiting to be accepted. It must still be
/// answered; a connection that stops accepting streams leaves the client waiting for a response that never comes
/// and keeps the connection (and the generation that owns it) open until the client gives up.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_waiting_to_be_accepted_when_the_service_shuts_down_is_answered() {
    let mut options = HttpServerOptions::default();
    options.h2c = true;
    let app = Arc::new(Answering { options });
    // Which of the two the server looks at first varies, so repeat until a lost request would show.
    for round in 0..40 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (accepted, _) = listener.accept().await.unwrap();
        let (shutdown, watch) = watch::channel(false);

        let (sender, connection) = h2::client::handshake(client).await.unwrap();
        tokio::spawn(connection);
        let request = http::Request::get("http://example.test/").body(()).unwrap();
        let mut ready = sender.ready().await.unwrap();
        let (response, _) = ready.send_request(request, true).unwrap();
        // Let the client connection write its preface and the request into the socket.
        tokio::time::sleep(Duration::from_millis(20)).await;
        shutdown.send(true).unwrap();

        let server = tokio::spawn({
            let app = app.clone();
            async move {
                app.process_new(Box::new(L4Stream::from(accepted)), &watch)
                    .await
            }
        });
        let response = timeout(Duration::from_secs(5), response)
            .await
            .unwrap_or_else(|_| panic!("round {round}: the request was never answered"))
            .unwrap();
        assert_eq!(response.status(), 200);
        drop(ready);
        timeout(Duration::from_secs(5), server)
            .await
            .unwrap_or_else(|_| panic!("round {round}: the connection stayed open"))
            .unwrap();
    }
}
