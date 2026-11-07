#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn absolute_form(target: &oxiroute_forward_proxy::ForwardTarget) -> Option<Uri> {
    let path = target
        .origin_form
        .path_and_query()
        .map_or(target.origin_form.path(), |value| value.as_str());
    Uri::builder()
        .scheme(target.scheme.as_str())
        .authority(target.destination.authority())
        .path_and_query(path)
        .build()
        .ok()
}

pub(super) fn apply_header_policy(headers: &mut HeaderMap, policy: &ForwardHeaderPolicy) {
    if policy.forwarded_for == ForwardedForPolicy::Delete {
        headers.remove(header::FORWARDED);
        headers.remove("x-forwarded-for");
    }
    if policy.via == ForwardViaPolicy::Delete {
        headers.remove(header::VIA);
    }
}

pub(super) fn sanitize_response_headers(headers: &mut HeaderMap) -> Result<(), ()> {
    let mut nominated = Vec::new();
    for value in headers.get_all(header::CONNECTION) {
        let value = value.to_str().map_err(|_| ())?;
        for name in value.split(',') {
            nominated.push(name.trim().parse::<http::HeaderName>().map_err(|_| ())?);
        }
    }
    for name in nominated {
        headers.remove(name);
    }
    for name in [
        header::CONNECTION,
        header::PROXY_AUTHENTICATE,
        header::PROXY_AUTHORIZATION,
        header::TE,
        header::TRAILER,
        header::TRANSFER_ENCODING,
        header::UPGRADE,
    ] {
        headers.remove(name);
    }
    headers.remove("keep-alive");
    headers.remove("proxy-connection");
    Ok(())
}

pub(super) fn header_has_token(
    headers: &HeaderMap,
    name: &http::HeaderName,
    expected: &str,
) -> bool {
    headers.get_all(name).iter().any(|value| {
        value.to_str().ok().is_some_and(|value| {
            value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case(expected))
        })
    })
}

pub(super) fn switching_protocols_response() -> Response<ForwardProxyBody> {
    let mut output = response(StatusCode::SWITCHING_PROTOCOLS, Bytes::new());
    output
        .headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
    output
        .headers_mut()
        .insert(header::UPGRADE, HeaderValue::from_static("connect-udp"));
    output
        .headers_mut()
        .insert(CAPSULE_PROTOCOL, HeaderValue::from_static("?1"));
    output
}

pub(super) fn response(status: StatusCode, body: Bytes) -> Response<ForwardProxyBody> {
    Response::builder()
        .status(status)
        .body(
            Full::new(body)
                .map_err(|never: Infallible| -> BoxError { match never {} })
                .boxed(),
        )
        .expect("static forward response")
}

fn cidr_contains(network: &str, address: IpAddr) -> bool {
    let Some((base, prefix)) = network.split_once('/') else {
        return false;
    };
    let (Ok(base), Ok(prefix)) = (base.parse::<IpAddr>(), prefix.parse::<u8>()) else {
        return false;
    };
    match (base, address) {
        (IpAddr::V4(base), IpAddr::V4(address)) if prefix <= 32 => {
            let host_bits = 32 - u32::from(prefix);
            let mask = if host_bits == 32 {
                0
            } else {
                u32::MAX << host_bits
            };
            u32::from(base) & mask == u32::from(address) & mask
        }
        (IpAddr::V6(base), IpAddr::V6(address)) if prefix <= 128 => {
            let host_bits = 128 - u32::from(prefix);
            let mask = if host_bits == 128 {
                0
            } else {
                u128::MAX << host_bits
            };
            u128::from(base) & mask == u128::from(address) & mask
        }
        _ => false,
    }
}

pub(super) fn source_cidrs_match(cidrs: &[String], client_addr: Option<SocketAddr>) -> bool {
    client_addr.is_some_and(|client_addr| {
        let address = match client_addr.ip() {
            IpAddr::V6(address) => address
                .to_ipv4_mapped()
                .map_or(IpAddr::V6(address), IpAddr::V4),
            IpAddr::V4(address) => IpAddr::V4(address),
        };
        cidrs.iter().any(|network| cidr_contains(network, address))
    })
}

pub(super) const fn is_link_local(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => address.is_link_local(),
        IpAddr::V6(address) => (address.segments()[0] & 0xffc0) == 0xfe80,
    }
}

pub(super) const fn protocol_name(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Http1 => "h1",
        Protocol::Http2 => "h2",
        Protocol::Http3 => "h3",
    }
}
