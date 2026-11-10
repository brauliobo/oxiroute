//! The pool and server inventories written straight from the health snapshots.
//!
//! A listing at a thousand servers is most of a megabyte, and the serde path (snapshot, DTO, `serde_json`) spent its
//! time on the same constant keys and on allocating decimal counters. These writers emit the keys as literals; the
//! field names, their order and the value encodings are those of [`ServerInventoryResponse`], [`PoolInventoryResponse`]
//! and [`PoolDto`], which stay the contract (schemas) and the reference the tests compare these bytes with.
use serde::Serialize;

use super::*;
use crate::wire::decimal;

/// Roughly the JSON of one server, so the buffer is not regrown while a listing is written.
const SERVER_BYTES: usize = 800;

pub(crate) fn servers_body(pools: &[PoolHealthSnapshot]) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        32 + SERVER_BYTES * pools.iter().map(|pool| pool.endpoints.len()).sum::<usize>(),
    );
    out.extend_from_slice(b"{\"servers\":[");
    let mut first = true;
    for pool in pools {
        for server in &pool.endpoints {
            separate(&mut out, &mut first);
            out.extend_from_slice(b"{\"pool\":");
            string(&mut out, &pool.name);
            out.extend_from_slice(b",\"server\":");
            endpoint(&mut out, server);
            out.push(b'}');
        }
    }
    out.extend_from_slice(b"]}");
    out
}

pub(crate) fn pools_body(pools: &[PoolHealthSnapshot]) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        32 + SERVER_BYTES
            * pools
                .iter()
                .map(|pool| pool.endpoints.len() + 1)
                .sum::<usize>(),
    );
    out.extend_from_slice(b"{\"pools\":");
    pools_array(&mut out, pools);
    out.push(b'}');
    out
}

/// The JSON array of [`PoolDto`], as the `upstreamPools` of the monitoring response carries it.
pub(crate) fn pools_json(pools: &[PoolHealthSnapshot]) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        SERVER_BYTES
            * pools
                .iter()
                .map(|pool| pool.endpoints.len() + 1)
                .sum::<usize>(),
    );
    pools_array(&mut out, pools);
    out
}

fn pools_array(out: &mut Vec<u8>, pools: &[PoolHealthSnapshot]) {
    out.push(b'[');
    let mut first = true;
    for pool in pools {
        separate(out, &mut first);
        self::pool(out, pool);
    }
    out.push(b']');
}

fn pool(out: &mut Vec<u8>, pool: &PoolHealthSnapshot) {
    let mut object = Object::start(out);
    object.string("name", &pool.name);
    object.string("algorithm", pool.algorithm);
    object.number("availableEndpoints", count(pool.available_endpoints));
    object.number("totalEndpoints", count(pool.total_endpoints));
    object.counter("unavailableSelections", pool.unavailable_selections);
    object.counter("failOpenSelections", pool.fail_open_selections);
    object.number("queued", pool.queued);
    object.counter("queuedTotal", pool.queued_total);
    object.counter("queueTimeouts", pool.queue_timeouts);
    object.counter("queueCancellations", pool.queue_cancellations);
    object.key("endpoints").push(b'[');
    let mut first = true;
    for server in &pool.endpoints {
        separate(object.out, &mut first);
        endpoint(object.out, server);
    }
    object.out.push(b']');
    object.finish();
}

fn endpoint(out: &mut Vec<u8>, server: &EndpointHealthSnapshot) {
    let mut object = Object::start(out);
    object.counter("activeConnections", server.active_connections);
    object.value(
        "administrativeState",
        &AdministrativeStateDto::from(server.administrative_state),
    );
    object.string("address", &server.address.to_string());
    object.boolean("checksEnabled", server.checks_enabled);
    object.boolean("checksRunning", server.checks_running);
    object.optional(
        "configuredMaxConnections",
        server.configured_max_connections,
    );
    object.value(
        "healthOverride",
        &HealthOverrideDto::from(server.health_override),
    );
    object.optional("maxConnections", server.max_connections);
    object.string("name", &server.name);
    object.value("origin", &server.origin);
    object.value("state", &EndpointHealthStateDto::from(server.state));
    object.number("weight", u64::from(server.weight));
    object.optional("lastCheckedAtUnixMs", server.last_checked_at_unix_ms);
    object.optional("lastTransitionAtUnixMs", server.last_transition_at_unix_ms);
    object.counter("successfulChecks", server.successful_checks);
    object.counter("failedChecks", server.failed_checks);
    object.counter("consecutiveSuccesses", server.consecutive_successes);
    object.counter("consecutiveFailures", server.consecutive_failures);
    object.value(
        "lastFailure",
        &server.last_failure.map(HealthFailureDto::from),
    );
    object.boolean("passiveEjected", server.passive_ejected);
    object.counter("passiveFailureCount", server.passive_failure_count);
    object.counter(
        "passiveConsecutiveFailures",
        server.passive_consecutive_failures,
    );
    object.counter("passiveEjectionCount", server.passive_ejection_count);
    object.value(
        "passiveEjectionReason",
        &server.passive_ejection_reason.map(HealthFailureDto::from),
    );
    object.optional("passiveEjectedAtUnixMs", server.passive_ejected_at_unix_ms);
    object.optional(
        "passiveEjectionUntilUnixMs",
        server.passive_ejection_until_unix_ms,
    );
    object.counter("passiveRecoveryCount", server.passive_recovery_count);
    object.optional(
        "passiveLastRecoveryAtUnixMs",
        server.passive_last_recovery_at_unix_ms,
    );
    object.finish();
}

fn separate(out: &mut Vec<u8>, first: &mut bool) {
    if !std::mem::take(first) {
        out.push(b',');
    }
}

/// One JSON object: the keys are written as they are, so they must not need escaping.
struct Object<'a> {
    out: &'a mut Vec<u8>,
    first: bool,
}

impl<'a> Object<'a> {
    fn start(out: &'a mut Vec<u8>) -> Self {
        out.push(b'{');
        Self { out, first: true }
    }

    fn key(&mut self, name: &'static str) -> &mut Vec<u8> {
        separate(self.out, &mut self.first);
        self.out.push(b'"');
        self.out.extend_from_slice(name.as_bytes());
        self.out.extend_from_slice(b"\":");
        self.out
    }

    fn finish(self) {
        self.out.push(b'}');
    }

    fn string(&mut self, name: &'static str, value: &str) {
        string(self.key(name), value);
    }

    /// A `u64` the API writes as a decimal string (`DecimalCounter`).
    fn counter(&mut self, name: &'static str, value: u64) {
        let out = self.key(name);
        out.push(b'"');
        out.extend_from_slice(decimal(value, &mut [0; 20]).as_bytes());
        out.push(b'"');
    }

    fn number(&mut self, name: &'static str, value: u64) {
        self.key(name)
            .extend_from_slice(decimal(value, &mut [0; 20]).as_bytes());
    }

    fn optional(&mut self, name: &'static str, value: Option<u64>) {
        match value {
            Some(value) => self.number(name, value),
            None => self.key(name).extend_from_slice(b"null"),
        }
    }

    fn boolean(&mut self, name: &'static str, value: bool) {
        self.key(name)
            .extend_from_slice(if value { b"true" } else { b"false" });
    }

    /// Enumerations and options of them keep the encoding their DTO type gives them.
    fn value(&mut self, name: &'static str, value: &impl Serialize) {
        serde_json::to_writer(self.key(name), value).expect("a Vec grows");
    }
}

/// A JSON string; text with nothing to escape, which is every name and address the API validates, is copied.
fn string(out: &mut Vec<u8>, value: &str) {
    if value
        .bytes()
        .all(|byte| byte >= 0x20 && byte != b'"' && byte != b'\\')
    {
        out.push(b'"');
        out.extend_from_slice(value.as_bytes());
        out.push(b'"');
    } else {
        serde_json::to_writer(out, value).expect("a Vec grows");
    }
}

fn count(value: usize) -> u64 {
    u64::try_from(value).expect("usize fits u64")
}
