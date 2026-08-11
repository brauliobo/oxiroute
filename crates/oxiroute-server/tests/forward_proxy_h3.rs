#![allow(dead_code, unused_imports, clippy::duplicate_mod)]

#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/process.rs"]
mod process_support;
#[path = "support/mod.rs"]
mod support;

use std::{net::Ipv4Addr, time::Duration};

use bytes::Bytes;
use http::{Method, Request, StatusCode};
use oxiroute_config::{
    AlpnProtocol, CacheKeyComponent, CacheStore, Certificate, CertificateSource,
    DownstreamTimeoutPolicy, ForwardAuditMode, ForwardConnectPolicy, ForwardDestinationPolicy,
    ForwardHeaderPolicy, ForwardHttpVersion, ForwardPeerPolicy, ForwardProxyService,
    ForwardResolverPolicy, HttpCachePolicy, Listener, ListenerBind, Protocol, TlsProfile,
    TlsVersion,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    time::timeout,
};

use support::h3::{H3_ALPN, client_endpoint, connect, drive_client, recv_chunk};

#[path = "forward_proxy_h3/absolute_form.rs"]
mod absolute_form;
#[path = "forward_proxy_h3/extended_connect.rs"]
mod extended_connect;
