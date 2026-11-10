#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! An origin whose responses are always stale (`Cache-Control: max-age=0`) and carry an `ETag`, so a route with a
//! cache revalidates on every request after the first: the origin answers 304 to a matching `If-None-Match` and
//! 200 with a body otherwise. It keeps connections alive and records the head of every request.

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use oxiroute_config::{
    CacheAuthorizationPolicy, CacheKeyComponent, CacheSetCookiePolicy, CacheStore, CacheVaryPolicy,
    ConfigDraft, HttpCachePolicy, HttpRouteAction,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    task::JoinHandle,
};

pub const BODY_LENGTH: usize = 1024;
pub const ETAG: &str = "\"v1\"";

pub fn body() -> Vec<u8> {
    vec![b'x'; BODY_LENGTH]
}

pub struct RevalidatingOrigin {
    pub address: SocketAddr,
    pub heads: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl RevalidatingOrigin {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let heads = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&heads);
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.expect("origin accept");
                let seen = Arc::clone(&seen);
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let mut buffer = [0_u8; 4096];
                    loop {
                        while let Some(end) =
                            bytes.windows(4).position(|window| window == b"\r\n\r\n")
                        {
                            let head = String::from_utf8_lossy(&bytes[..end + 4]).into_owned();
                            bytes.drain(..end + 4);
                            let reply = reply(&head);
                            seen.lock().expect("heads").push(head);
                            if stream.write_all(&reply).await.is_err() {
                                return;
                            }
                        }
                        match stream.read(&mut buffer).await {
                            Ok(0) | Err(_) => return,
                            Ok(count) => bytes.extend_from_slice(&buffer[..count]),
                        }
                    }
                });
            }
        });
        Self {
            address,
            heads,
            task,
        }
    }

    pub fn heads(&self) -> Vec<String> {
        self.heads.lock().expect("heads").clone()
    }

    /// Requests the origin saw, after the first one all conditional.
    pub fn assert_revalidated(&self, requests: usize) {
        let heads = self.heads();
        assert_eq!(
            heads.len(),
            requests,
            "one origin request per client request"
        );
        let conditional =
            |head: &String| head.to_ascii_lowercase().contains("if-none-match: \"v1\"");
        assert!(!conditional(&heads[0]), "{}", heads[0]);
        assert!(heads[1..].iter().all(conditional), "{heads:?}");
    }

    pub fn finish(self) {
        self.task.abort();
    }
}

fn reply(head: &str) -> Vec<u8> {
    if head
        .to_ascii_lowercase()
        .contains(&format!("if-none-match: {ETAG}"))
    {
        format!("HTTP/1.1 304 Not Modified\r\nETag: {ETAG}\r\nCache-Control: max-age=0\r\n\r\n")
            .into_bytes()
    } else {
        let head = format!(
            "HTTP/1.1 200 OK\r\nETag: {ETAG}\r\nCache-Control: max-age=0\r\nContent-Length: {BODY_LENGTH}\r\n\r\n"
        );
        [head.into_bytes(), body()].concat()
    }
}

/// A memory cache on the first route of the first service, keyed by scheme, host, path and query.
pub fn with_cache(config: &mut ConfigDraft) {
    config.cache_stores = vec![CacheStore::memory("memory", 1024 * 1024)];
    let HttpRouteAction::Proxy { policy, .. } = &mut config.http_services[0].routes[0].action
    else {
        unreachable!("wire route is a proxy route");
    };
    policy.cache = Some(Box::new(HttpCachePolicy {
        store: "memory".into(),
        methods: vec!["GET".into(), "HEAD".into()],
        key_components: vec![
            CacheKeyComponent::Scheme,
            CacheKeyComponent::NormalizedHost,
            CacheKeyComponent::PathAndQuery,
        ],
        use_origin_cache_control: true,
        default_ttl_ms: 60_000,
        status_ttls: Vec::new(),
        grace_ms: 30_000,
        keep_ms: 300_000,
        revalidate: true,
        collapsed_forwarding: true,
        stale_on: Vec::new(),
        bypass_request: Vec::new(),
        no_store_request: Vec::new(),
        no_store_response: Vec::new(),
        set_cookie_policy: CacheSetCookiePolicy::default(),
        authorization_policy: CacheAuthorizationPolicy::default(),
        vary_policy: CacheVaryPolicy::default(),
        surrogate_tags: None,
        purge_authorization: None,
    }));
}
