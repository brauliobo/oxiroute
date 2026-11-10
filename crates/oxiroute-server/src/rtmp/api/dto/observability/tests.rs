use std::collections::BTreeSet;

use oxiroute_rtmp::{PreparedRtmpRuntimeSet, RtmpPrepareContext, RtmpPrepareMode, RtmpRuntimeSet};
use schemars::generate::SchemaSettings;
use serde_json::{Value, json};

use super::*;
use crate::RuntimeMetrics;

fn schema<T: JsonSchema>() -> Value {
    let generator = SchemaSettings::draft2020_12().into_generator();
    serde_json::to_value(generator.into_root_schema_for::<T>()).expect("response schema")
}

fn empty_rtmp_runtime() -> RtmpRuntimeSet {
    PreparedRtmpRuntimeSet::prepare(
        [],
        &RtmpPrepareContext::new(RtmpPrepareMode::Activation, []),
        std::time::Instant::now() + std::time::Duration::from_secs(1),
    )
    .expect("empty RTMP preparation")
    .start(std::time::Instant::now() + std::time::Duration::from_secs(1))
    .expect("empty RTMP runtime")
}

#[test]
fn monitoring_projection_preserves_the_runtime_v1_wire_object() {
    let runtime = RuntimeMetrics::new().snapshot().expect("runtime snapshot");
    let legacy_runtime = serde_json::to_value(&runtime).expect("legacy runtime JSON");
    let control = empty_rtmp_runtime().control();
    let projected = MonitoringResponse::project(runtime, &control.catalog_snapshot(), false)
        .expect("monitoring projection");
    let projected = serde_json::to_value(projected).expect("projected monitoring JSON");

    for (key, value) in legacy_runtime.as_object().expect("runtime object") {
        assert_eq!(&projected[key], value, "runtime field {key}");
    }
    assert_eq!(projected["rtmp"]["relays"], json!([]));
    assert_eq!(projected["rtmp"]["recorders"], json!([]));
}

#[test]
fn monitoring_body_is_the_serialized_response_byte_for_byte() {
    let control = empty_rtmp_runtime().control();
    for pools in [0, 2] {
        let mut runtime = RuntimeMetrics::new().snapshot().expect("runtime snapshot");
        runtime.upstream_pools = (0..pools)
            .map(|index| crate::PoolHealthSnapshot {
                name: format!("pool-{index}"),
                algorithm: "round_robin",
                available_endpoints: 0,
                total_endpoints: 0,
                unavailable_selections: u64::MAX,
                fail_open_selections: 1,
                queued: 2,
                queued_by_class: Vec::new(),
                oldest_queue_wait_ms: 0,
                queued_total: 3,
                queue_timeouts: 4,
                queue_cancellations: 5,
                canary: None,
                endpoints: Vec::new(),
            })
            .collect();
        let response = MonitoringResponse::project(runtime, &control.catalog_snapshot(), false)
            .expect("monitoring projection");

        assert_eq!(
            response.body().expect("direct body"),
            serde_json::to_vec(&response).expect("serialized response")
        );
    }
}

#[test]
fn monitoring_projection_owns_every_runtime_snapshot_field() {
    let runtime = RuntimeMetrics::new().snapshot().expect("runtime snapshot");
    let control = empty_rtmp_runtime().control();
    let value = serde_json::to_value(
        MonitoringResponse::project(runtime, &control.catalog_snapshot(), false)
            .expect("monitoring projection"),
    )
    .expect("monitoring JSON");
    let actual = value
        .as_object()
        .expect("monitoring object")
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let expected = [
        "accessRecords",
        "acmeManagedCertificates",
        "certbotCertificates",
        "certbotWatcher",
        "directFileCertificates",
        "directFileWatcher",
        "generationAgeMs",
        "host",
        "listeners",
        "process",
        "rtmp",
        "sampledAtUnixMs",
        "traffic",
        "transportOperations",
        "upstreamPools",
        "uptimeMs",
    ]
    .into_iter()
    .collect();
    assert_eq!(actual, expected);
}

#[test]
fn observability_schemas_are_structural_and_keep_decimal_counters_as_strings() {
    for response in [
        schema::<StatusResponse>(),
        schema::<ReadinessResponse>(),
        schema::<CapabilitiesResponse>(),
        schema::<MonitoringResponse>(),
    ] {
        let encoded = response.to_string();
        assert!(!encoded.contains("serde_json::Value"));
        assert!(!encoded.contains("\"additionalProperties\":true"));
    }
    let monitoring = schema::<MonitoringResponse>();
    assert_eq!(monitoring["$defs"]["DecimalCounter"]["type"], "string");
    assert_eq!(
        monitoring["$defs"]["RelayMonitoringRowDto"]["properties"]["queueBytes"]["$ref"],
        "#/$defs/DecimalCounter"
    );
    assert_eq!(
        monitoring["$defs"]["RecorderMonitoringRowDto"]["properties"]["bytesWritten"]["$ref"],
        "#/$defs/DecimalCounter"
    );
}

#[test]
fn capability_projection_matches_the_existing_v1_golden_shape() {
    let value = serde_json::to_value(
        CapabilitiesResponse::project(&[], RuntimeMode::Direct).expect("capabilities projection"),
    )
    .expect("capabilities JSON");
    assert_eq!(
        value,
        crate::http3::capability_snapshot(&[], RuntimeMode::Direct)
    );
}

#[test]
fn public_observability_shapes_do_not_expose_secret_bearing_fields() {
    let schemas = [schema::<StatusResponse>(), schema::<MonitoringResponse>()]
        .into_iter()
        .map(|schema| schema.to_string().to_ascii_lowercase())
        .collect::<String>();
    for forbidden in ["token", "password", "privatekey", "authorization"] {
        assert!(!schemas.contains(forbidden), "schema exposed {forbidden}");
    }
}
