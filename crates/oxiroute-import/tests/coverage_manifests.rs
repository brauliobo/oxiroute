#![cfg(unix)]

#[path = "coverage_manifests/canonical_schema.rs"]
mod canonical_schema;
#[path = "coverage_manifests/component_gates.rs"]
mod component_gates;
#[path = "coverage_manifests/haproxy_probes.rs"]
mod haproxy_probes;
#[path = "coverage_manifests/host_ledger.rs"]
mod host_ledger;
#[path = "coverage_manifests/manifests.rs"]
mod manifests;
#[path = "coverage_manifests/nginx_probes.rs"]
mod nginx_probes;
#[path = "coverage_manifests/report_invariants.rs"]
mod report_invariants;
#[path = "coverage_manifests/squid_probes.rs"]
mod squid_probes;
#[path = "coverage_manifests/support.rs"]
mod support;
