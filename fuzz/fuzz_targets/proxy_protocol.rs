#![no_main]

mod support;

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use libfuzzer_sys::fuzz_target;
use oxiroute_config::{ProxyProtocolPolicy, ProxyProtocolVersion};
use oxiroute_server::{ProxyProtocolTransport, accept_stream, encode_header, parse_header};
use tokio::{io::AsyncReadExt, sync::watch};
use tokio_test::io::Builder;

const MAX_INPUT_BYTES: usize = 131_072;
const MAX_FRAGMENTS: usize = 128;

fuzz_target!(|data: &[u8]| {
    let Some(data) = support::bounded_input(data, MAX_INPUT_BYTES) else {
        return;
    };
    let data = data.as_ref();
    let (version, input) = select_version(data);

    let complete = parse_header(input, version, ProxyProtocolTransport::Stream)
        .ok()
        .flatten()
        .is_some();
    let _ = parse_header(
        input,
        ProxyProtocolVersion::Auto,
        ProxyProtocolTransport::Stream,
    );

    let (source, destination) = addresses(data);
    for version in [ProxyProtocolVersion::V1, ProxyProtocolVersion::V2] {
        if let Ok(mut encoded) =
            encode_header(version, ProxyProtocolTransport::Stream, source, destination)
        {
            encoded.extend_from_slice(b"payload");
            let _ = parse_header(&encoded, version, ProxyProtocolTransport::Stream);
        }
    }

    if complete {
        let mut builder = Builder::new();
        let fragment_size = fragment_size(input, MAX_FRAGMENTS);
        for fragment in input.chunks(fragment_size).take(MAX_FRAGMENTS) {
            builder.read(fragment);
        }
        let stream = builder.build();
        let (_shutdown_sender, mut shutdown) = watch::channel(false);
        let policy = ProxyProtocolPolicy {
            version,
            timeout_ms: 100,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime");
        runtime.block_on(async move {
            if let Ok(mut accepted) = accept_stream(stream, policy, &mut shutdown).await {
                let mut payload = Vec::new();
                let _ = accepted.stream.read_to_end(&mut payload).await;
            }
        });
    }
});

fn fragment_size(input: &[u8], maximum_fragments: usize) -> usize {
    let varied = usize::from(input.first().copied().unwrap_or_default() % 31) + 1;
    let minimum = input.len().div_ceil(maximum_fragments.max(1));
    varied.max(minimum)
}

fn select_version(data: &[u8]) -> (ProxyProtocolVersion, &[u8]) {
    for (prefix, version) in [
        (b"v1:".as_slice(), ProxyProtocolVersion::V1),
        (b"v2:".as_slice(), ProxyProtocolVersion::V2),
        (b"auto:".as_slice(), ProxyProtocolVersion::Auto),
    ] {
        if let Some(input) = support::strip_prefix(data, prefix) {
            return (version, input);
        }
    }

    if data.starts_with(b"PROXY ") {
        return (ProxyProtocolVersion::V1, data);
    }
    if data.starts_with(b"\r\n\r\n\0\r\nQUIT\n") {
        return (ProxyProtocolVersion::V2, data);
    }

    let selector = data.first().copied().unwrap_or_default() % 3;
    let input = data.get(1..).unwrap_or_default();
    let version = match selector {
        0 => ProxyProtocolVersion::V1,
        1 => ProxyProtocolVersion::V2,
        _ => ProxyProtocolVersion::Auto,
    };
    (version, input)
}

fn addresses(data: &[u8]) -> (SocketAddr, SocketAddr) {
    let seed = data.first().copied().unwrap_or_default();
    let source_port = u16::from_be_bytes([
        data.get(1).copied().unwrap_or_default(),
        data.get(2).copied().unwrap_or_default(),
    ]);
    let destination_port = u16::from_be_bytes([
        data.get(3).copied().unwrap_or_default(),
        data.get(4).copied().unwrap_or_default(),
    ]);
    if seed & 1 == 0 {
        (
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, seed)), source_port),
            SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(198, 51, 100, seed)),
                destination_port,
            ),
        )
    } else {
        (
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), source_port),
            SocketAddr::new(
                IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, u16::from(seed))),
                destination_port,
            ),
        )
    }
}
