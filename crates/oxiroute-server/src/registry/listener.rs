//! Registry-only listeners: group-permission unix sockets and a token-gated LAN endpoint.
//!
//! They serve the registry routes, and `/ready` and `/status.json` on the read socket, and nothing else, so a worker
//! can register without ever reaching the management API.

mod socket;

use std::{convert::Infallible, io, net::IpAddr, sync::Arc, thread, time::Duration};

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, LengthLimitError, Limited};
use hyper::{
    HeaderMap, Request, Response,
    body::Incoming,
    header::{ALLOW, AUTHORIZATION, CONNECTION, CONTENT_TYPE, RETRY_AFTER},
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use oxiroute_config::{RegistryEndpoint, RegistryRole, cidr_contains};
use serde_json::json;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpListener, UnixListener},
    runtime::Builder,
    sync::{Semaphore, watch},
    time::timeout,
};

use self::socket::{SocketFile, bind_tcp};
use super::{
    auth::{Principal, Scope},
    error::RegistryError,
    http::{self, MAX_BODY_BYTES, Reply, Route},
    service::RegistryService,
    status_feed::StatusFeed,
};

const HEADER_TIMEOUT: Duration = Duration::from_secs(5);
const BODY_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CONNECTIONS: usize = 128;

/// Who an endpoint serves and what its callers may do.
enum Access {
    Read,
    Register(Vec<String>),
    Lan(Vec<String>),
}

struct Endpoint {
    service: Arc<RegistryService>,
    feed: Arc<StatusFeed>,
    access: Access,
}

/// The remote end of one connection.
struct Caller {
    id: String,
    peer: Option<IpAddr>,
}

/// Running endpoints. Dropping the value stops them and removes their socket files.
pub(crate) struct RegistryListeners {
    stop: watch::Sender<bool>,
    _files: Vec<SocketFile>,
}

impl Drop for RegistryListeners {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}

impl RegistryListeners {
    /// Binds every endpoint before returning so a bad path or port is reported at start-up.
    ///
    /// # Errors
    ///
    /// Returns the first bind failure; nothing stays bound.
    pub(crate) fn start(
        service: &Arc<RegistryService>,
        feed: &Arc<StatusFeed>,
        endpoints: &[RegistryEndpoint],
    ) -> io::Result<Self> {
        let runtime = Builder::new_current_thread().enable_all().build()?;
        let (stop, stopped) = watch::channel(false);
        let mut files = Vec::new();
        let mut serving = Vec::new();
        for endpoint in endpoints {
            let access = access(endpoint);
            let context = Arc::new(Endpoint {
                service: Arc::clone(service),
                feed: Arc::clone(feed),
                access,
            });
            match endpoint {
                RegistryEndpoint::Unix { path, mode, .. } => {
                    let (listener, file) = SocketFile::bind(path, *mode)?;
                    files.push(file);
                    let listener = {
                        let _guard = runtime.enter();
                        UnixListener::from_std(listener)?
                    };
                    serving.push(Serving::Unix(listener, context));
                }
                RegistryEndpoint::Tcp { bind, .. } => {
                    let listener = {
                        let _guard = runtime.enter();
                        bind_tcp(*bind)?
                    };
                    serving.push(Serving::Tcp(listener, context));
                }
            }
        }
        thread::Builder::new()
            .name("registry-listeners".into())
            .spawn(move || {
                runtime.block_on(async move {
                    for listener in serving {
                        tokio::spawn(listener.run(stopped.clone()));
                    }
                    let mut stopped = stopped;
                    drop(stopped.wait_for(|stopped| *stopped).await);
                });
            })?;
        Ok(Self {
            stop,
            _files: files,
        })
    }
}

fn access(endpoint: &RegistryEndpoint) -> Access {
    match endpoint {
        RegistryEndpoint::Unix {
            role: RegistryRole::Read,
            ..
        } => Access::Read,
        RegistryEndpoint::Unix {
            role: RegistryRole::Register,
            pools,
            ..
        } => Access::Register(pools.clone()),
        RegistryEndpoint::Tcp { allow_cidrs, .. } => Access::Lan(allow_cidrs.clone()),
    }
}

enum Serving {
    Unix(UnixListener, Arc<Endpoint>),
    Tcp(TcpListener, Arc<Endpoint>),
}

impl Serving {
    async fn run(self, mut stopped: watch::Receiver<bool>) {
        let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        loop {
            let accepted = tokio::select! {
                accepted = self.accept() => accepted,
                _ = stopped.wait_for(|stopped| *stopped) => return,
            };
            let (connection, caller, endpoint) = match accepted {
                Ok(accepted) => accepted,
                Err(error) => {
                    log::warn!("registry endpoint accept failed: {error}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let Ok(slot) = Arc::clone(&slots).try_acquire_owned() else {
                continue;
            };
            tokio::spawn(async move {
                connection.serve(endpoint, caller).await;
                drop(slot);
            });
        }
    }

    async fn accept(&self) -> io::Result<(Connection, Arc<Caller>, Arc<Endpoint>)> {
        match self {
            Self::Unix(listener, endpoint) => {
                let (stream, _) = listener.accept().await?;
                let id = stream.peer_cred().map_or_else(
                    |_| "unix".to_owned(),
                    |credentials| format!("unix:uid={}", credentials.uid()),
                );
                let caller = Arc::new(Caller { id, peer: None });
                Ok((Connection::Unix(stream), caller, Arc::clone(endpoint)))
            }
            Self::Tcp(listener, endpoint) => {
                let (stream, peer) = listener.accept().await?;
                let caller = Arc::new(Caller {
                    id: peer.ip().to_canonical().to_string(),
                    peer: Some(peer.ip().to_canonical()),
                });
                Ok((Connection::Tcp(stream), caller, Arc::clone(endpoint)))
            }
        }
    }
}

enum Connection {
    Unix(tokio::net::UnixStream),
    Tcp(tokio::net::TcpStream),
}

impl Connection {
    async fn serve(self, endpoint: Arc<Endpoint>, caller: Arc<Caller>) {
        match self {
            Self::Unix(stream) => serve(stream, endpoint, caller).await,
            Self::Tcp(stream) => serve(stream, endpoint, caller).await,
        }
    }
}

async fn serve<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    stream: S,
    endpoint: Arc<Endpoint>,
    caller: Arc<Caller>,
) {
    let handler = service_fn(move |request| {
        let (endpoint, caller) = (Arc::clone(&endpoint), Arc::clone(&caller));
        async move { Ok::<_, Infallible>(response(endpoint.answer(&caller, request).await)) }
    });
    let connection = http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_TIMEOUT)
        .serve_connection(TokioIo::new(stream), handler);
    if let Err(error) = connection.await {
        log::debug!("registry connection ended: {error}");
    }
}

impl Endpoint {
    async fn answer(&self, caller: &Caller, request: Request<Incoming>) -> Reply {
        let (parts, body) = request.into_parts();
        if matches!(self.access, Access::Read) {
            match parts.uri.path() {
                "/ready" => return self.ready(&parts.method),
                "/status.json" => return self.status(&parts.method).await,
                _ => {}
            }
        }
        let principal = match self.authenticate(caller, &parts.headers) {
            Ok(principal) => principal,
            Err(error) => return Reply::error(&error),
        };
        let body = match read_body(body).await {
            Ok(body) => body,
            Err(error) => return Reply::error(&error),
        };
        let service = Arc::clone(&self.service);
        let peer = caller.peer;
        tokio::task::spawn_blocking(move || execute(&service, &parts, &body, &principal, peer))
            .await
            .unwrap_or_else(|_| Reply::error(&RegistryError::RegistryUnavailable))
    }

    async fn status(&self, method: &hyper::Method) -> Reply {
        if method != hyper::Method::GET {
            return Reply::error(&RegistryError::RouteNotFound);
        }
        let feed = Arc::clone(&self.feed);
        match tokio::task::spawn_blocking(move || feed.document()).await {
            Ok(Some(document)) => Reply::json(200, &document),
            _ => Reply::error(&RegistryError::RegistryUnavailable),
        }
    }

    fn ready(&self, method: &hyper::Method) -> Reply {
        if method != hyper::Method::GET {
            return Reply::error(&RegistryError::RouteNotFound);
        }
        let degraded = self.service.status().degraded.is_some();
        let state = if degraded { "degraded" } else { "ready" };
        Reply::json(
            if degraded { 503 } else { 200 },
            &json!({ "status": state }),
        )
    }

    fn authenticate(
        &self,
        caller: &Caller,
        headers: &HeaderMap,
    ) -> Result<Principal, RegistryError> {
        let principal = match &self.access {
            Access::Read => Principal {
                id: caller.id.clone(),
                scope: Scope::Read,
            },
            Access::Register(pools) => Principal {
                id: caller.id.clone(),
                scope: Scope::Register {
                    pools: if pools.is_empty() {
                        self.service.registration_pools()
                    } else {
                        pools.clone()
                    },
                    name_prefix: None,
                },
            },
            Access::Lan(cidrs) => {
                let peer = caller.peer.expect("tcp callers have a peer address");
                if !cidrs.is_empty()
                    && !cidrs
                        .iter()
                        .any(|cidr| cidr_contains(cidr, peer) == Some(true))
                {
                    return Err(RegistryError::PeerNotAllowed);
                }
                self.service.check_rate(&caller.id)?;
                headers
                    .get(AUTHORIZATION)
                    .and_then(|value| http::bearer(value.as_bytes()))
                    .and_then(|secret| self.service.authenticate(secret))
                    .ok_or(RegistryError::Unauthorized)?
            }
        };
        self.service.check_rate(&principal.id)?;
        Ok(principal)
    }
}

async fn read_body(body: Incoming) -> Result<Bytes, RegistryError> {
    match timeout(BODY_TIMEOUT, Limited::new(body, MAX_BODY_BYTES).collect()).await {
        Ok(Ok(collected)) => Ok(collected.to_bytes()),
        Ok(Err(error)) if error.is::<LengthLimitError>() => Err(RegistryError::BodyTooLarge),
        _ => Err(RegistryError::InvalidRequest(
            "the request body could not be read",
        )),
    }
}

fn execute(
    service: &RegistryService,
    parts: &hyper::http::request::Parts,
    body: &[u8],
    principal: &Principal,
    peer: Option<IpAddr>,
) -> Reply {
    let Some(route) = http::match_route(parts.uri.path()) else {
        return Reply::error(&RegistryError::RouteNotFound);
    };
    let reply = http::handle(
        service,
        route,
        &http::Request {
            method: parts.method.as_str(),
            query: parts.uri.query(),
            body,
            principal,
            peer,
        },
    );
    if !matches!(route, Route::Heartbeat { .. }) && parts.method != hyper::Method::GET {
        log::info!(
            "registry {} {} by {} from {} -> {}",
            parts.method,
            parts.uri.path(),
            principal.id,
            peer.map_or_else(|| "socket".to_owned(), |peer| peer.to_string()),
            reply.status
        );
    }
    reply
}

fn response(reply: Reply) -> Response<Full<Bytes>> {
    let mut builder = Response::builder().status(reply.status);
    if reply.body.is_some() {
        builder = builder.header(CONTENT_TYPE, "application/json");
    }
    if let Some(allow) = reply.allow {
        builder = builder.header(ALLOW, allow);
    }
    if let Some(seconds) = reply.retry_after_seconds {
        builder = builder.header(RETRY_AFTER, seconds);
    }
    if reply.close {
        builder = builder.header(CONNECTION, "close");
    }
    let body = reply
        .body
        .map(|value| serde_json::to_vec(&value).expect("a JSON value serializes"))
        .unwrap_or_default();
    builder
        .body(Full::new(Bytes::from(body)))
        .expect("registry response parts are valid")
}
