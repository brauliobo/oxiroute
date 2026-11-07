use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    record::{AnnouncedState, Registration},
    service::{RegistrationStatus, RegistrationView},
};
use crate::{AdministrativeState, EndpointHealthSnapshot, EndpointHealthState, ServerOrigin};

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnouncedStateDto {
    Ready,
    Draining,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdministrativeStateDto {
    Ready,
    Drain,
    Maintenance,
}

#[derive(Clone, Copy, Debug, JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginDto {
    Static,
    Dynamic,
}

#[derive(Clone, Copy, Debug, JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationStatusDto {
    Active,
    Shadowed,
    Orphaned,
}

#[derive(Clone, Copy, Debug, JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStateDto {
    Unchecked,
    Unknown,
    Healthy,
    Unhealthy,
}

/// A stored registration. `metadata` is withheld from register-only credentials.
#[derive(Clone, Debug, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationDto {
    pub pool: String,
    pub name: String,
    pub address: String,
    pub weight: u16,
    pub priority: u16,
    pub max_connections: Option<u64>,
    pub labels: BTreeMap<String, String>,
    pub metadata: Option<serde_json::Value>,
    pub ttl_ms: u64,
    pub announced_state: AnnouncedStateDto,
    pub operator_state: AdministrativeStateDto,
    pub effective_state: AdministrativeStateDto,
    pub registered_by: String,
}

impl RegistrationDto {
    pub fn new(record: &Registration, with_metadata: bool) -> Self {
        Self {
            pool: record.pool.clone(),
            name: record.name.clone(),
            address: record.address.to_string(),
            weight: record.weight,
            priority: record.priority,
            max_connections: record.max_connections,
            labels: record.labels.clone(),
            metadata: with_metadata.then(|| record.metadata.clone()),
            ttl_ms: record.ttl_ms,
            announced_state: match record.announced_state {
                AnnouncedState::Ready => AnnouncedStateDto::Ready,
                AnnouncedState::Draining => AnnouncedStateDto::Draining,
            },
            operator_state: administrative(record.operator_state),
            effective_state: administrative(record.effective_state()),
            registered_by: record.registered_by.clone(),
        }
    }
}

fn administrative(state: AdministrativeState) -> AdministrativeStateDto {
    match state {
        AdministrativeState::Ready => AdministrativeStateDto::Ready,
        AdministrativeState::Drain => AdministrativeStateDto::Drain,
        AdministrativeState::Maintenance => AdministrativeStateDto::Maintenance,
    }
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PutResponse {
    pub server: RegistrationDto,
    pub expires_in_ms: u64,
    pub ttl_ms: u64,
    pub registry_revision: String,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatResponse {
    pub expires_in_ms: u64,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationEntryDto {
    pub registration: RegistrationDto,
    pub expires_in_ms: Option<u64>,
    pub status: RegistrationStatusDto,
}

impl RegistrationEntryDto {
    pub fn new(view: &RegistrationView, with_metadata: bool) -> Self {
        Self {
            registration: RegistrationDto::new(&view.record, with_metadata),
            expires_in_ms: view.expires_in_ms,
            status: match view.status {
                RegistrationStatus::Active => RegistrationStatusDto::Active,
                RegistrationStatus::Shadowed => RegistrationStatusDto::Shadowed,
                RegistrationStatus::Orphaned => RegistrationStatusDto::Orphaned,
            },
        }
    }
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationsResponse {
    pub registrations: Vec<RegistrationEntryDto>,
    pub registry_revision: String,
    pub writer: bool,
    pub degraded: Option<String>,
}

/// A member of a pool, whether it comes from the file or from a registration.
#[derive(Clone, Debug, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolServerDto {
    pub name: String,
    pub origin: OriginDto,
    pub address: String,
    pub state: HealthStateDto,
    pub administrative_state: AdministrativeStateDto,
    pub weight: u16,
    pub max_connections: Option<u64>,
    pub active_connections: String,
    pub expires_in_ms: Option<u64>,
    pub registration: Option<RegistrationDto>,
}

impl PoolServerDto {
    pub fn new(
        member: &EndpointHealthSnapshot,
        view: Option<&RegistrationView>,
        with_metadata: bool,
    ) -> Self {
        Self {
            name: member.name.clone(),
            origin: match member.origin {
                ServerOrigin::Static => OriginDto::Static,
                ServerOrigin::Dynamic => OriginDto::Dynamic,
            },
            address: member.address.to_string(),
            state: match member.state {
                EndpointHealthState::Unchecked => HealthStateDto::Unchecked,
                EndpointHealthState::Unknown => HealthStateDto::Unknown,
                EndpointHealthState::Healthy => HealthStateDto::Healthy,
                EndpointHealthState::Unhealthy => HealthStateDto::Unhealthy,
            },
            administrative_state: administrative(member.administrative_state),
            weight: member.weight,
            max_connections: member.max_connections,
            active_connections: member.active_connections.to_string(),
            expires_in_ms: view.and_then(|view| view.expires_in_ms),
            registration: view.map(|view| RegistrationDto::new(&view.record, with_metadata)),
        }
    }
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolServersResponse {
    pub pool: String,
    pub servers: Vec<PoolServerDto>,
    pub registry_revision: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OperatorStateRequest {
    pub state: AdministrativeStateDto,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
}
