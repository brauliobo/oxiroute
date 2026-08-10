#![no_main]

mod support;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use libfuzzer_sys::fuzz_target;
use oxiroute_config::ProxyProtocolVersion;
use oxiroute_server::{ProxyProtocolTransport, encode_header, parse_header};

const MAX_UDP_PAYLOAD_BYTES: usize = 65_507;
const MAX_INPUT_BYTES: usize = 131_059;

fuzz_target!(|data: &[u8]| {
    let Some(mut data) =
        support::bounded_input(data, MAX_INPUT_BYTES).map(|data| data.into_owned())
    else {
        return;
    };
    if data == b"seed:oversized" {
        let header = encode_header(
            ProxyProtocolVersion::V2,
            ProxyProtocolTransport::Datagram,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)), 1234),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 20)), 443),
        )
        .expect("fixed datagram header");
        data = header;
        data.extend(std::iter::repeat_n(
            0,
            MAX_UDP_PAYLOAD_BYTES.saturating_add(1),
        ));
    }

    let (version, input) = select_version(&data);
    let _ = parse_header(input, version, ProxyProtocolTransport::Datagram);
    let _ = parse_header(
        input,
        ProxyProtocolVersion::Auto,
        ProxyProtocolTransport::Datagram,
    );

    let header = encode_header(
        ProxyProtocolVersion::V2,
        ProxyProtocolTransport::Datagram,
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 1),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1)), 2),
    )
    .expect("fixed datagram header");
    let payload = &data[..data.len().min(MAX_UDP_PAYLOAD_BYTES)];
    let mut framed = Vec::with_capacity(header.len() + payload.len());
    framed.extend_from_slice(&header);
    framed.extend_from_slice(payload);
    let _ = parse_header(
        &framed,
        ProxyProtocolVersion::V2,
        ProxyProtocolTransport::Datagram,
    );
});

fn select_version(data: &[u8]) -> (ProxyProtocolVersion, &[u8]) {
    if let Some(input) = support::strip_prefix(data, b"v1:") {
        (ProxyProtocolVersion::V1, input)
    } else if let Some(input) = support::strip_prefix(data, b"v2:") {
        (ProxyProtocolVersion::V2, input)
    } else if data.starts_with(b"\r\n\r\n\0\r\nQUIT\n") {
        (ProxyProtocolVersion::V2, data)
    } else {
        let version = match data.first().copied().unwrap_or_default() % 3 {
            0 => ProxyProtocolVersion::V1,
            1 => ProxyProtocolVersion::V2,
            _ => ProxyProtocolVersion::Auto,
        };
        (version, data)
    }
}
