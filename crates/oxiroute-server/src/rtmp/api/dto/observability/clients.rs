use std::{
    collections::HashMap,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use schemars::JsonSchema;
use serde::Serialize;

use crate::{
    clients::{
        Bucket, ClientQuery, ClientRow, ClientsView, ConnectionQuery, ConnectionRow,
        ConnectionsView, Registry, Sort,
    },
    http::inflight::CurrentRequest,
};

const CLIENTS_DEFAULT_LIMIT: usize = 200;
const CLIENTS_MAX_LIMIT: usize = 2_000;
const CONNECTIONS_DEFAULT_LIMIT: usize = 500;
const CONNECTIONS_MAX_LIMIT: usize = 5_000;

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClientListResponse {
    sampled_at_unix_ms: u64,
    generation: u64,
    ipv6_prefix: u8,
    tracked_clients: u64,
    truncated: bool,
    clients: Vec<ClientDto>,
    other: BucketDto,
    totals: BucketDto,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClientDto {
    ip: String,
    connections: u64,
    connections_total: u64,
    requests: u64,
    bytes_in: u64,
    bytes_out: u64,
    first_seen_unix_ms: u64,
    last_activity_unix_ms: u64,
    listeners: Vec<String>,
    protocols: Vec<&'static str>,
    errors: u64,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct BucketDto {
    clients: u64,
    connections: u64,
    requests: u64,
    bytes_in: u64,
    bytes_out: u64,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnectionListResponse {
    sampled_at_unix_ms: u64,
    truncated: bool,
    total: u64,
    connections: Vec<ConnectionDto>,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionDto {
    id: u64,
    peer_ip: String,
    peer_port: u16,
    listener: String,
    protocol: &'static str,
    tls: Option<TlsDto>,
    state: &'static str,
    age_ms: u64,
    idle_ms: u64,
    requests: u64,
    bytes_in: u64,
    bytes_out: u64,
    service: Option<String>,
    upstream: Option<String>,
    current_request: Option<CurrentRequestDto>,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct TlsDto {
    version: &'static str,
    sni: Option<String>,
    alpn: Option<String>,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct CurrentRequestDto {
    method: String,
    path: String,
    age_ms: u64,
}

/// What the active generation says about a listener of the connections it serves.
pub(crate) struct ListenerFacts {
    pub(crate) service: Option<String>,
    pub(crate) draining: bool,
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

fn percent_decoded(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let pair = value.get(index + 1..index + 3)?;
            decoded.push(u8::from_str_radix(pair, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn parse_limit(value: &str, max: usize) -> Result<usize, &'static str> {
    value
        .parse()
        .ok()
        .filter(|limit| (1..=max).contains(limit))
        .ok_or(match max {
            CLIENTS_MAX_LIMIT => "limit must be between 1 and 2000",
            _ => "limit must be between 1 and 5000",
        })
}

fn parse_sort(value: &str) -> Result<Sort, &'static str> {
    match value {
        "bytes" => Ok(Sort::Bytes),
        "connections" => Ok(Sort::Connections),
        "requests" => Ok(Sort::Requests),
        "recent" => Ok(Sort::Recent),
        _ => Err("sort must be bytes, connections, requests or recent"),
    }
}

fn pairs(query: Option<&str>) -> impl Iterator<Item = (&str, &str)> {
    query
        .unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| pair.split_once('=').unwrap_or((pair, "")))
}

fn clients_query(query: Option<&str>) -> Result<ClientQuery, &'static str> {
    let mut parsed = ClientQuery {
        limit: CLIENTS_DEFAULT_LIMIT,
        sort: Sort::Bytes,
        ip_prefix: None,
    };
    for (name, value) in pairs(query) {
        match name {
            "limit" => parsed.limit = parse_limit(value, CLIENTS_MAX_LIMIT)?,
            "sort" => parsed.sort = parse_sort(value)?,
            "ip" => parsed.ip_prefix = Some(percent_decoded(value).ok_or("invalid ip prefix")?),
            _ => return Err("unknown query parameter"),
        }
    }
    Ok(parsed)
}

fn connections_query(query: Option<&str>) -> Result<ConnectionQuery, &'static str> {
    let mut parsed = ConnectionQuery {
        limit: CONNECTIONS_DEFAULT_LIMIT,
        ip_prefix: None,
        listener: None,
    };
    for (name, value) in pairs(query) {
        match name {
            "limit" => parsed.limit = parse_limit(value, CONNECTIONS_MAX_LIMIT)?,
            "ip" => parsed.ip_prefix = Some(percent_decoded(value).ok_or("invalid ip prefix")?),
            "listener" => {
                parsed.listener = Some(percent_decoded(value).ok_or("invalid listener name")?);
            }
            _ => return Err("unknown query parameter"),
        }
    }
    Ok(parsed)
}

fn age_before(now_ms: u64, age: Duration) -> u64 {
    now_ms.saturating_sub(u64::try_from(age.as_millis()).unwrap_or(u64::MAX))
}

impl From<Bucket> for BucketDto {
    fn from(bucket: Bucket) -> Self {
        Self {
            clients: bucket.clients,
            connections: bucket.connections,
            requests: bucket.requests,
            bytes_in: bucket.bytes_in,
            bytes_out: bucket.bytes_out,
        }
    }
}

impl ClientDto {
    fn new(row: ClientRow, now_ms: u64) -> Self {
        Self {
            ip: row.ip.to_string(),
            connections: row.connections,
            connections_total: row.connections_total,
            requests: row.requests,
            bytes_in: row.bytes_in,
            bytes_out: row.bytes_out,
            first_seen_unix_ms: age_before(now_ms, row.first_seen_age),
            last_activity_unix_ms: age_before(now_ms, row.last_activity_age),
            listeners: row.listeners.iter().map(ToString::to_string).collect(),
            protocols: row.protocols,
            errors: row.errors,
        }
    }
}

impl ClientListResponse {
    pub(crate) fn current(
        query: Option<&str>,
        registry: &Registry,
        generation: u64,
    ) -> Result<Self, &'static str> {
        let query = clients_query(query)?;
        let ClientsView {
            settings,
            tracked_clients,
            truncated,
            clients,
            other,
            totals,
        } = registry.clients(&query, Instant::now());
        let now_ms = unix_ms();
        Ok(Self {
            sampled_at_unix_ms: now_ms,
            generation,
            ipv6_prefix: settings.ipv6_prefix,
            tracked_clients,
            truncated,
            clients: clients
                .into_iter()
                .map(|row| ClientDto::new(row, now_ms))
                .collect(),
            other: other.into(),
            totals: totals.into(),
        })
    }
}

impl ConnectionListResponse {
    pub(crate) fn current(
        query: Option<&str>,
        registry: &Registry,
        listeners: &HashMap<String, ListenerFacts>,
        mut current: HashMap<u64, CurrentRequest>,
    ) -> Result<Self, &'static str> {
        let query = connections_query(query)?;
        let ConnectionsView {
            total,
            truncated,
            connections,
        } = registry.connections(&query, Instant::now());
        Ok(Self {
            sampled_at_unix_ms: unix_ms(),
            truncated,
            total,
            connections: connections
                .into_iter()
                .map(|row| {
                    let request = current.remove(&row.id);
                    ConnectionDto::new(row, listeners, request)
                })
                .collect(),
        })
    }
}

impl ConnectionDto {
    fn new(
        row: ConnectionRow,
        listeners: &HashMap<String, ListenerFacts>,
        request: Option<CurrentRequest>,
    ) -> Self {
        let facts = listeners.get(row.listener.as_ref());
        let state = Self::state(&row, facts, request.is_some());
        let upstream = row
            .upstream
            .as_deref()
            .map(str::to_owned)
            .or_else(|| request.as_ref().and_then(|request| request.server.clone()));
        Self {
            id: row.id,
            peer_ip: row.peer.ip().to_canonical().to_string(),
            peer_port: row.peer.port(),
            listener: row.listener.to_string(),
            protocol: row.protocol.as_str(),
            tls: row.tls.map(|tls| TlsDto {
                version: tls.version,
                sni: tls.sni,
                alpn: tls.alpn,
            }),
            state,
            age_ms: row.age_ms,
            idle_ms: row.idle_ms,
            requests: row.requests,
            bytes_in: row.bytes_in,
            bytes_out: row.bytes_out,
            service: facts.and_then(|facts| facts.service.clone()),
            upstream,
            current_request: request.map(|request| CurrentRequestDto {
                method: request.method,
                path: request.path,
                age_ms: request.age_ms,
            }),
        }
    }

    fn state(row: &ConnectionRow, facts: Option<&ListenerFacts>, serving: bool) -> &'static str {
        if row.handshaking {
            "handshaking"
        } else if facts.is_some_and(|facts| facts.draining) {
            "draining"
        } else if serving || !row.protocol.serves_requests() {
            "active"
        } else {
            "idle"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clients::Protocol;

    #[test]
    fn queries_default_and_reject_unknown_names_and_bad_values() {
        let defaults = clients_query(None).expect("defaults");
        assert_eq!((defaults.limit, defaults.sort), (200, Sort::Bytes));
        assert_eq!(connections_query(Some("")).expect("defaults").limit, 500);
        for query in ["limit=0", "limit=2001", "sort=speed", "other=1", "ip=%zz"] {
            assert!(clients_query(Some(query)).is_err(), "{query}");
        }
        assert!(connections_query(Some("limit=5001")).is_err());
        assert!(connections_query(Some("sort=bytes")).is_err());
    }

    #[test]
    fn an_encoded_ipv6_prefix_is_decoded() {
        let parsed = clients_query(Some("ip=2001%3Adb8&limit=3&sort=recent")).expect("query");
        assert_eq!(parsed.ip_prefix.as_deref(), Some("2001:db8"));
        assert_eq!((parsed.limit, parsed.sort), (3, Sort::Recent));
    }

    #[test]
    fn a_connection_state_follows_the_handshake_the_drain_and_the_request() {
        let row = |protocol, handshaking| ConnectionRow {
            id: 1,
            peer: "203.0.113.7:4000".parse().expect("address"),
            listener: "edge".into(),
            protocol,
            tls: None,
            handshaking,
            age_ms: 0,
            idle_ms: 0,
            requests: 0,
            bytes_in: 0,
            bytes_out: 0,
            upstream: None,
        };
        let draining = ListenerFacts {
            service: None,
            draining: true,
        };
        assert_eq!(
            ConnectionDto::state(&row(Protocol::Http1, true), None, false),
            "handshaking"
        );
        assert_eq!(
            ConnectionDto::state(&row(Protocol::Http1, false), Some(&draining), true),
            "draining"
        );
        assert_eq!(
            ConnectionDto::state(&row(Protocol::Http1, false), None, true),
            "active"
        );
        assert_eq!(
            ConnectionDto::state(&row(Protocol::Http1, false), None, false),
            "idle"
        );
        assert_eq!(
            ConnectionDto::state(&row(Protocol::Tcp, false), None, false),
            "active"
        );
    }
}
