use std::net::{IpAddr, SocketAddr};

use oxiroute_config::PoolRegistration;

use super::{
    error::RegistryError,
    record::{HeartbeatRequest, Registration, RegistrationRequest},
};
use crate::AdministrativeState;

const MAX_NAME_BYTES: usize = 64;
const MAX_LABELS: usize = 16;
const MAX_LABEL_KEY_BYTES: usize = 64;
const MAX_LABEL_VALUE_BYTES: usize = 256;
const MAX_METADATA_BYTES: usize = 2048;
const MAX_WEIGHT: u16 = 100;
const DEFAULT_WEIGHT: u16 = 1;
const DEFAULT_PRIORITY: u16 = 100;

/// Where a registration request came from.
pub struct Requester<'a> {
    /// Token id or socket role recorded as `registeredBy`.
    pub id: &'a str,
    /// Remote IP for TCP requests; unix sockets have none.
    pub peer: Option<IpAddr>,
}

pub fn validate_name(name: &str) -> Result<(), RegistryError> {
    let lexical = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-');
    if name.is_empty() || name.len() > MAX_NAME_BYTES || !name.bytes().all(lexical) {
        return Err(RegistryError::InvalidName);
    }
    Ok(())
}

pub fn validate_put(
    pool: &str,
    name: &str,
    policy: &PoolRegistration,
    protected: &[SocketAddr],
    requester: &Requester<'_>,
    request: RegistrationRequest,
) -> Result<Registration, RegistryError> {
    validate_name(name)?;
    let address = request
        .address
        .ok_or(RegistryError::InvalidAddress("address is required"))?;
    check_address(address, policy, protected, requester)?;
    let weight = request.weight.unwrap_or(DEFAULT_WEIGHT);
    if !(1..=MAX_WEIGHT).contains(&weight) {
        return Err(RegistryError::InvalidRequest(
            "weight must be between 1 and 100",
        ));
    }
    if request.max_connections == Some(0) {
        return Err(RegistryError::InvalidRequest(
            "maxConnections must be positive",
        ));
    }
    check_metadata(&request.labels, &request.metadata)?;
    Ok(Registration {
        pool: pool.to_owned(),
        name: name.to_owned(),
        address,
        weight,
        priority: request.priority.unwrap_or(DEFAULT_PRIORITY),
        max_connections: request.max_connections,
        labels: request.labels,
        metadata: request.metadata,
        ttl_ms: ttl_ms(policy, request.ttl_ms)?,
        announced_state: request.state,
        operator_state: AdministrativeState::Ready,
        registered_by: requester.id.to_owned(),
    })
}

/// Applies the optional heartbeat fields to a stored record.
pub fn apply_heartbeat(
    policy: &PoolRegistration,
    mut record: Registration,
    request: HeartbeatRequest,
) -> Result<Registration, RegistryError> {
    if let Some(state) = request.state {
        record.announced_state = state;
    }
    if let Some(labels) = request.labels {
        record.labels = labels;
    }
    if let Some(metadata) = request.metadata {
        record.metadata = metadata;
    }
    if request.ttl_ms.is_some() {
        record.ttl_ms = ttl_ms(policy, request.ttl_ms)?;
    }
    check_metadata(&record.labels, &record.metadata)?;
    Ok(record)
}

fn ttl_ms(policy: &PoolRegistration, requested: Option<u64>) -> Result<u64, RegistryError> {
    let ttl = requested.unwrap_or(policy.default_ttl_ms);
    if (policy.min_ttl_ms..=policy.max_ttl_ms).contains(&ttl) {
        Ok(ttl)
    } else {
        Err(RegistryError::InvalidTtl)
    }
}

fn check_metadata(
    labels: &std::collections::BTreeMap<String, String>,
    metadata: &serde_json::Value,
) -> Result<(), RegistryError> {
    let labels_ok = labels.len() <= MAX_LABELS
        && labels.iter().all(|(key, value)| {
            !key.is_empty()
                && key.len() <= MAX_LABEL_KEY_BYTES
                && value.len() <= MAX_LABEL_VALUE_BYTES
        });
    let metadata_ok =
        serde_json::to_vec(metadata).is_ok_and(|bytes| bytes.len() <= MAX_METADATA_BYTES);
    if labels_ok && metadata_ok {
        Ok(())
    } else {
        Err(RegistryError::InvalidMetadata)
    }
}

fn check_address(
    address: SocketAddr,
    policy: &PoolRegistration,
    protected: &[SocketAddr],
    requester: &Requester<'_>,
) -> Result<(), RegistryError> {
    let ip = address.ip();
    if address.port() == 0 || ip.is_unspecified() || ip.is_multicast() {
        return Err(RegistryError::InvalidAddress(
            "address must be a unicast host and a non-zero port",
        ));
    }
    let allowed = policy.allows(ip);
    if !allowed && (ip.is_loopback() || is_link_local(ip)) {
        return Err(RegistryError::AddressNotAllowed(
            "loopback and link-local addresses need an explicit allowed_cidrs entry",
        ));
    }
    if !policy.allowed_cidrs.is_empty() && !allowed {
        return Err(RegistryError::AddressNotAllowed(
            "address is outside allowed_cidrs",
        ));
    }
    if protected.iter().any(|protected| {
        protected.port() == address.port()
            && (protected.ip() == ip || protected.ip().is_unspecified())
    }) {
        return Err(RegistryError::AddressNotAllowed(
            "address is an oxiroute listener",
        ));
    }
    if policy.require_peer_address && requester.peer.is_some_and(|peer| peer != ip) {
        return Err(RegistryError::AddressNotAllowed(
            "address must be the registering peer",
        ));
    }
    Ok(())
}

fn is_link_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_link_local(),
        IpAddr::V6(ip) => (ip.segments()[0] & 0xffc0) == 0xfe80,
    }
}

#[cfg(test)]
mod tests;
