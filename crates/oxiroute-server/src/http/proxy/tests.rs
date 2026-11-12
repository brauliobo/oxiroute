use std::{
    net::SocketAddr,
    os::fd::AsRawFd as _,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use http::header::{SET_COOKIE, TE, UPGRADE};

use oxiroute_config::{
    HttpCookieAttributePolicy, HttpCookiePathRewrite, HttpProxyPathRewrite, HttpProxyPolicy,
    HttpRedirectLocation, HttpRequestHeaderMutation, HttpRequestHeaderValue,
    HttpResponseHeaderMutation, HttpRetryPolicy, HttpSameSite, UpstreamAlgorithm,
    UpstreamConnectionReuse,
};
use pingora::{
    protocols::{GetSocketDigest as _, SocketDigest, Stream},
    proxy::Session,
    upstreams::peer::Peer,
};
use tokio::io::AsyncWriteExt as _;
use tokio::net::{TcpListener, TcpStream};

use super::*;

use crate::{PassiveFailurePolicy, RoundRobinPool, RouteTable, RuntimeMetrics};

struct CloneProbe(Arc<AtomicUsize>);

impl Clone for CloneProbe {
    fn clone(&self) -> Self {
        self.0.fetch_add(1, Ordering::Relaxed);
        Self(Arc::clone(&self.0))
    }
}

async fn request_preparation_fixture(
    policy: HttpProxyPolicy,
    connection_reuse: UpstreamConnectionReuse,
    request: &[u8],
) -> (HttpReverseProxy, Session, HttpRequestContext, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("request preparation listener");
    let client = TcpStream::connect(listener.local_addr().expect("preparation address"));
    let accept = listener.accept();
    let (client, downstream) = tokio::join!(client, accept);
    let mut client = client.expect("request preparation client");
    let (downstream, _) = downstream.expect("request preparation connection");
    client
        .write_all(request)
        .await
        .expect("write downstream request");
    let mut downstream = pingora::protocols::l4::stream::Stream::from(downstream);
    downstream.set_socket_digest(SocketDigest::from_raw_fd(downstream.as_raw_fd()));
    let mut session = Session::new_h1(Box::new(downstream));
    session
        .read_request()
        .await
        .expect("parse downstream request");

    let endpoint = RuntimeEndpoint::Socket {
        address: listener.local_addr().expect("preparation endpoint"),
    };
    let selector = Arc::new(
        RoundRobinPool::new_named(
            "preparation".into(),
            [endpoint],
            UpstreamAlgorithm::RoundRobin,
            false,
        )
        .expect("preparation selector"),
    );
    let plan = Arc::new(UpstreamPlan::with_policy(
        selector,
        None,
        None,
        None,
        connection_reuse,
    ));
    let route = Arc::new(HttpRoutePlan {
        access: None,
        lane: crate::http::proxy::lane::LaneRoute::Classic,
        edits: crate::http::proxy::lane::RouteEdits::default(),
        action: HttpActionPlan::Proxy(crate::http::action::ProxyActionPlan {
            pool: Arc::clone(&plan),
            rescue_pool: None,
            select: None,
            policy: Box::new(ProxyPolicyPlan::compile(&policy)),
        }),
        policy: crate::http::action::RoutePolicyPlan::compile(
            oxiroute_config::HttpRoutePolicy::default(),
        ),
        route_id: "preparation".into(),
    });
    let runtime = RuntimeMetrics::new();
    let metrics = runtime
        .register_listener("preparation", "http", "127.0.0.1:8080", 10)
        .expect("preparation metrics");
    let service = Arc::new(HttpServicePlan::new(
        Some(1024),
        Vec::new(),
        Duration::from_secs(1),
        RouteTable::default(),
    ));
    let proxy = HttpReverseProxy::new(service, metrics);
    let mut context = proxy.new_ctx();
    context.request_facts = session.request_facts();
    context.authority = Some("example.test".parse().expect("request authority"));
    context.pool = Some(plan);
    context.route = Some(route);
    context.selected_upstream_host = Some(HeaderValue::from_static("example.test"));
    context.request_header_decisions = pingora_request_header_decisions(
        &session,
        context.route.as_ref().expect("request route"),
        context.authority.as_ref(),
    )
    .expect("request header decisions");
    (proxy, session, context, client)
}

#[cfg(unix)]
async fn unix_request_session(request: &[u8]) -> (Session, tokio::net::UnixStream) {
    use tokio::net::{UnixListener, UnixStream};

    let directory = tempfile::tempdir().expect("Unix request directory");
    let path = directory.path().join("downstream.sock");
    let listener = UnixListener::bind(&path).expect("Unix request listener");
    let client = UnixStream::connect(&path);
    let accept = listener.accept();
    let (client, downstream) = tokio::join!(client, accept);
    let mut client = client.expect("Unix request client");
    let (downstream, _) = downstream.expect("Unix request connection");
    client
        .write_all(request)
        .await
        .expect("write Unix downstream request");
    let mut session = Session::new_h1(Box::new(downstream));
    session
        .read_request()
        .await
        .expect("parse Unix downstream request");
    (session, client)
}

mod evacuation;
mod request;
mod response;
mod upstream;
