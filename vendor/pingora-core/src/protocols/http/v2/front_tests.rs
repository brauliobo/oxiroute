//! The HTTP/2 front of the accept loop: what it is asked, what it may claim and what stays with the session.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use http::{HeaderMap, Method, Request, StatusCode};
use pingora_http::{RequestHeader, ResponseHeader};
use tokio::io::duplex;
use tokio::sync::watch;

use super::server::{self, handshake, Dispatch, H2Stream};
use crate::apps::{ClaimedStream, Http2Claim, Http2Front};
use crate::protocols::Digest;
use crate::server::ShutdownWatch;

/// Claims `/claimed` and serves it with a `204`; hands everything else back.
struct TestFront {
    asked: AtomicUsize,
}

impl Http2Front for TestFront {
    fn claim(
        self: Arc<Self>,
        request: RequestHeader,
        mut stream: H2Stream,
        _digest: &Digest,
        _shutdown: &ShutdownWatch,
    ) -> Http2Claim {
        self.asked.fetch_add(1, Ordering::SeqCst);
        if request.uri.path() != "/claimed" {
            return Http2Claim::Declined { request, stream };
        }
        Http2Claim::Claimed(ClaimedStream::new(move || async move {
            stream
                .send_response(StatusCode::NO_CONTENT, HeaderMap::new(), true)
                .unwrap();
        }))
    }
}

/// Serves one connection with the front: claimed streams run their future, the sessions the loop hands out answer `200`.
/// Returns how many sessions the loop handed out.
async fn serve(io: tokio::io::DuplexStream, front: Arc<TestFront>) -> usize {
    let connection = handshake(Box::new(io), None).await.unwrap();
    let (_stop, shutdown) = watch::channel(false);
    let sessions = Arc::new(AtomicUsize::new(0));
    let counted = sessions.clone();
    let front: Arc<dyn Http2Front> = front;
    server::accept_downstream_dispatch(
        connection,
        Arc::new(Digest::default()),
        shutdown,
        None,
        Some(front),
        move |dispatched, guard| match dispatched {
            Dispatch::Session(mut session) => {
                counted.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let _guard = guard;
                    let head = Box::new(ResponseHeader::build(200, None).unwrap());
                    session.write_response_header(head, true).unwrap();
                });
            }
            Dispatch::Claimed(claimed) => {
                tokio::spawn(async move {
                    let _guard = guard;
                    claimed.await;
                });
            }
        },
    )
    .await;
    sessions.load(Ordering::SeqCst)
}

/// The statuses the client gets for `uris`, one request each on one connection.
async fn statuses(client: tokio::io::DuplexStream, uris: &[&str]) -> Vec<StatusCode> {
    let (h2, connection) = h2::client::handshake(client).await.unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let mut h2 = h2.ready().await.unwrap();
    let mut codes = Vec::new();
    for uri in uris {
        let request = Request::builder()
            .method(Method::GET)
            .uri(*uri)
            .body(())
            .unwrap();
        let (response, _) = h2.send_request(request, true).unwrap();
        codes.push(response.await.unwrap().status());
        h2 = h2.ready().await.unwrap();
    }
    codes
}

#[tokio::test]
async fn a_claimed_request_is_served_by_the_front_and_the_rest_by_sessions() {
    let (client, server) = duplex(65536);
    let front = Arc::new(TestFront {
        asked: AtomicUsize::new(0),
    });
    let served = tokio::spawn(serve(server, front.clone()));
    let codes = statuses(
        client,
        &[
            "https://example.com/claimed",
            "https://example.com/other",
            "https://example.com/claimed",
        ],
    )
    .await;
    assert_eq!(
        codes,
        [
            StatusCode::NO_CONTENT,
            StatusCode::OK,
            StatusCode::NO_CONTENT
        ]
    );
    assert_eq!(
        served.await.unwrap(),
        1,
        "only the declined request is a session"
    );
    assert_eq!(front.asked.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn a_request_the_acceptance_checks_reject_is_not_offered_to_the_front() {
    let (client, server) = duplex(65536);
    let front = Arc::new(TestFront {
        asked: AtomicUsize::new(0),
    });
    let served = tokio::spawn(serve(server, front.clone()));
    // Userinfo in the authority: the loop answers it with a 400 before any front or session sees it.
    let codes = statuses(client, &["https://user@example.com/claimed"]).await;
    assert_eq!(codes, [StatusCode::BAD_REQUEST]);
    assert_eq!(served.await.unwrap(), 0);
    assert_eq!(front.asked.load(Ordering::SeqCst), 0);
}
