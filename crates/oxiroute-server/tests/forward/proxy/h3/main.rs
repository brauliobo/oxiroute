#![allow(dead_code, unused_imports, clippy::duplicate_mod)]

#[path = "../../../support/fixtures.rs"]
mod fixture_support;
#[path = "../../../support/process.rs"]
mod process_support;
#[path = "../../../support/mod.rs"]
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

mod absolute_form;
mod extended_connect;
