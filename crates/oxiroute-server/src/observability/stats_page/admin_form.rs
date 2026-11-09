//! The administration form of the stats page: what it accepts and which requests may post it.

use std::{collections::HashMap, net::IpAddr};

use http::{
    HeaderMap, Uri,
    header::{CONTENT_LENGTH, CONTENT_TYPE, HOST, ORIGIN, REFERER},
    uri::Authority,
};
use pingora::protocols::http::ServerSession;

use crate::{
    ApiResponse,
    observability::stats::transport_ip_is_loopback,
    secure_bearer::{HeaderCardinality, single_header},
};

const MAX_STATS_FORM_BYTES: usize = 8 * 1024;

pub(super) struct StatsPageAdminTarget {
    pub(super) generation_revision: String,
    pub(super) pool: String,
    pub(super) server: String,
    pub(super) state: String,
}

pub(super) fn form_content_type(headers: &HeaderMap) -> bool {
    matches!(
        single_header(headers, &CONTENT_TYPE),
        HeaderCardinality::Single(value)
            if value
                .to_str()
                .ok()
                .and_then(|value| value.split(';').next())
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/x-www-form-urlencoded"))
    )
}

pub(super) fn request_is_same_origin(headers: &HeaderMap) -> bool {
    if [
        "forwarded",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
        "x-real-ip",
        "x-client-ip",
    ]
    .iter()
    .any(|name| headers.contains_key(*name))
    {
        return false;
    }
    let HeaderCardinality::Single(host) = single_header(headers, &HOST) else {
        return false;
    };
    let Ok(host) = host.to_str() else {
        return false;
    };
    let Ok(host) = host.parse::<Authority>() else {
        return false;
    };
    if !authority_is_loopback(&host) {
        return false;
    }
    match single_header(headers, &ORIGIN) {
        HeaderCardinality::Single(origin) => origin
            .to_str()
            .ok()
            .and_then(|origin| origin.parse::<Uri>().ok())
            .is_some_and(|origin| {
                origin.scheme_str() == Some("http")
                    && origin.query().is_none()
                    && origin.path() == "/"
                    && origin
                        .authority()
                        .is_some_and(|origin| origin.as_str().eq_ignore_ascii_case(host.as_str()))
            }),
        HeaderCardinality::Missing => matches!(
            single_header(headers, &REFERER),
            HeaderCardinality::Single(referer)
                if referer
                    .to_str()
                    .ok()
                    .and_then(|referer| referer.parse::<Uri>().ok())
                    .is_some_and(|referer| {
                        referer.scheme_str() == Some("http")
                            && referer.authority().is_some_and(|referer| {
                                referer.as_str().eq_ignore_ascii_case(host.as_str())
                            })
                    })
        ),
        HeaderCardinality::Duplicate => false,
    }
}

pub(super) fn authority_is_loopback(authority: &Authority) -> bool {
    if authority.as_str().contains('@') || !authority_has_valid_optional_port(authority.as_str()) {
        return false;
    }
    let host = authority.host();
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(transport_ip_is_loopback)
}

fn authority_has_valid_optional_port(authority: &str) -> bool {
    let port = if let Some(bracket) = authority.strip_prefix('[') {
        let Some(end) = bracket.find(']') else {
            return false;
        };
        let remainder = &bracket[end + 1..];
        if remainder.is_empty() {
            return true;
        }
        remainder.strip_prefix(':')
    } else {
        match authority.matches(':').count() {
            0 => return true,
            1 => authority.rsplit_once(':').map(|(_, port)| port),
            _ => return false,
        }
    };
    port.is_some_and(|port| !port.is_empty() && port.parse::<u16>().is_ok())
}

pub(super) async fn read_stats_form_body(
    session: &mut ServerSession,
) -> Result<Vec<u8>, Box<ApiResponse>> {
    let mut content_lengths = session.req_header().headers.get_all(CONTENT_LENGTH).iter();
    let declared_length = content_lengths
        .next()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    if content_lengths.next().is_some() || declared_length.is_none() {
        return Err(Box::new(ApiResponse::error(
            400,
            "invalid_content_length",
            "exactly one decimal Content-Length value is required",
        )));
    }
    if declared_length.is_some_and(|length| length > MAX_STATS_FORM_BYTES) {
        return Err(stats_form_too_large().into());
    }
    let mut body = Vec::with_capacity(declared_length.unwrap_or_default());
    while let Some(chunk) = session.read_request_body().await.map_err(|_| {
        Box::new(ApiResponse::error(
            400,
            "invalid_request_body",
            "request body could not be read",
        ))
    })? {
        if chunk.len() > MAX_STATS_FORM_BYTES.saturating_sub(body.len()) {
            return Err(stats_form_too_large().into());
        }
        body.extend_from_slice(&chunk);
    }
    if body.len() != declared_length.unwrap_or_default() {
        return Err(Box::new(ApiResponse::error(
            400,
            "invalid_request_body",
            "request body length does not match Content-Length",
        )));
    }
    Ok(body)
}

fn stats_form_too_large() -> ApiResponse {
    ApiResponse::error(
        413,
        "form_too_large",
        format!("stats administration form exceeds the {MAX_STATS_FORM_BYTES}-byte limit"),
    )
}

pub(super) fn parse_stats_form(body: &[u8]) -> Result<StatsPageAdminTarget, ()> {
    let mut fields = HashMap::new();
    for pair in body.split(|byte| *byte == b'&') {
        let separator = pair.iter().position(|byte| *byte == b'=').ok_or(())?;
        let (name, value) = (&pair[..separator], &pair[separator + 1..]);
        let name = decode_form_component(name)?;
        if !matches!(
            name.as_str(),
            "generation_revision" | "pool" | "server" | "state"
        ) || fields.insert(name, decode_form_component(value)?).is_some()
        {
            return Err(());
        }
    }
    Ok(StatsPageAdminTarget {
        generation_revision: fields
            .remove("generation_revision")
            .filter(|v| !v.is_empty())
            .ok_or(())?,
        pool: fields.remove("pool").filter(|v| !v.is_empty()).ok_or(())?,
        server: fields
            .remove("server")
            .filter(|v| !v.is_empty())
            .ok_or(())?,
        state: fields.remove("state").filter(|v| !v.is_empty()).ok_or(())?,
    })
}

fn decode_form_component(value: &[u8]) -> Result<String, ()> {
    let mut decoded = Vec::with_capacity(value.len());
    let mut index = 0;
    while index < value.len() {
        match value[index] {
            b'+' => decoded.push(b' '),
            b'%' => {
                let high = *value.get(index + 1).ok_or(())?;
                let low = *value.get(index + 2).ok_or(())?;
                decoded.push(form_hex(high)? << 4 | form_hex(low)?);
                index += 2;
            }
            byte => decoded.push(byte),
        }
        index += 1;
    }
    let decoded = String::from_utf8(decoded).map_err(|_| ())?;
    if decoded.chars().any(char::is_control) {
        return Err(());
    }
    Ok(decoded)
}

const fn form_hex(value: u8) -> Result<u8, ()> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(()),
    }
}
