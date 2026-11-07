#![allow(dead_code, unused_imports, clippy::duplicate_mod)]

#[path = "../support/fixtures.rs"]
mod fixture_support;
#[path = "../support/http.rs"]
mod http_support;
#[path = "../support/process.rs"]
mod process_support;
#[path = "../support/mod.rs"]
mod support;

use std::{
    fs,
    io::BufReader,
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use bytes::{Buf as _, Bytes, BytesMut};
use h3::{client::RequestStream, error::Code, proto::coding::BufMutExt as _, server::Connection};
use http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode};
use oxiroute_config::{
    AlpnProtocol, CacheKeyComponent, CacheStore, Certificate, CertificateSource, ConfigDraft,
    DownstreamTimeoutPolicy, HttpCookieAttributePolicy, HttpCookiePathRewrite, HttpPathSelector,
    HttpProxyPolicy, HttpRedirectLocation, HttpRequestHeaderMutation, HttpRequestHeaderValue,
    HttpResponseHeaderMutation, HttpRoute, HttpRouteAction, HttpRoutePolicy, HttpSameSite,
    HttpService, HttpStaticMimePolicy, HttpStaticPathMapping, HttpVersion, HttpVersionPolicy,
    Listener, ListenerBind, Management, Protocol, TlsProfile, TlsVersion, UpstreamAlgorithm,
    UpstreamPool, UpstreamTls,
};
use quinn::crypto::rustls::{HandshakeData, QuicClientConfig, QuicServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use tokio::{
    io::AsyncWriteExt as _,
    time::{sleep, timeout},
};

mod helpers;
mod limits;
mod reload;
mod serving;
mod static_files;

#[allow(clippy::wildcard_imports)]
use helpers::*;
#[allow(clippy::wildcard_imports)]
use reload::*;

const H3_ALPN: &[u8] = b"h3";
const TEST_TOKEN: &str = "2d9e0b7f5c4a3e1d8f6b0a9c7e5d3b1f2a4c6e8d0b9f7a5c3e1d9b7f5a3c1e8d";
