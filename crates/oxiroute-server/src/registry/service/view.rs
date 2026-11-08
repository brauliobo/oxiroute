use std::collections::BTreeMap;

use serde::Serialize;

use crate::registry::record::Registration;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationStatus {
    /// The server is a member of the live pool.
    Active,
    /// A static server with the same name or endpoint wins; the record is kept but inactive.
    Shadowed,
    /// The pool no longer opts into registration or no longer exists; the record is kept.
    Orphaned,
}

#[derive(Clone, Debug)]
pub struct RegistrationView {
    pub record: Registration,
    pub expires_in_ms: Option<u64>,
    pub status: RegistrationStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PutOutcome {
    pub created: bool,
    pub record: Registration,
    pub expires_in_ms: u64,
    pub registry_revision: u64,
    pub warnings: Vec<&'static str>,
}

#[derive(Clone, Debug)]
pub struct RegistryStatus {
    pub revision: u64,
    pub writer: bool,
    pub degraded: Option<String>,
    pub records: usize,
}

/// Counters for the metrics page.
#[derive(Clone, Debug, Default)]
pub struct RegistryMetrics {
    pub operations: BTreeMap<(&'static str, &'static str), u64>,
    pub expirations: BTreeMap<String, u64>,
    pub rejections: BTreeMap<&'static str, u64>,
    pub store_errors: u64,
    pub revision: u64,
    pub servers: BTreeMap<(String, &'static str), u64>,
    /// Milliseconds left on each lease this process tracks, by pool and server.
    pub lease_remaining_ms: BTreeMap<(String, String), u64>,
}
