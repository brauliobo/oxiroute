//! The registry API contract, generated from the same types the handlers use.

use std::{collections::BTreeMap, fs, path::PathBuf};

use schemars::generate::SchemaSettings;
use serde_json::{Map, Value, json};

use super::{
    dto::{
        ErrorBody, HeartbeatResponse, OperatorStateRequest, PoolServersResponse, PutResponse,
        RegistrationsResponse,
    },
    record::{HeartbeatRequest, RegistrationRequest},
};

const ARTIFACT_RELATIVE_PATH: &str = "../../contracts/registry.openapi.json";
const SERVER: &str = "/api/v1/pools/{pool}/servers/{name}";

fn generated_openapi() -> Value {
    let mut settings = SchemaSettings::draft2020_12().for_serialize();
    settings.definitions_path = "/components/schemas".into();
    settings.meta_schema = None;
    let mut generator = settings.into_generator();
    generator.subschema_for::<RegistrationRequest>();
    generator.subschema_for::<HeartbeatRequest>();
    generator.subschema_for::<OperatorStateRequest>();
    generator.subschema_for::<PutResponse>();
    generator.subschema_for::<HeartbeatResponse>();
    generator.subschema_for::<PoolServersResponse>();
    generator.subschema_for::<RegistrationsResponse>();
    generator.subschema_for::<ErrorBody>();
    let schemas = generator
        .take_definitions(false)
        .into_iter()
        .collect::<Map<_, _>>();

    let paths = json!({
        "/api/v1/registrations": {
            "get": operation("listRegistrations", "Every registration with its lease and status.", None, &[("200", Some("RegistrationsResponse"))], "read"),
        },
        "/api/v1/pools/{pool}/servers": {
            "parameters": [path_parameter("pool")],
            "get": operation("listPoolServers", "Static and registered members of a pool.", None, &[("200", Some("PoolServersResponse"))], "read"),
        },
        SERVER: {
            "parameters": [path_parameter("pool"), path_parameter("name")],
            "get": operation("getPoolServer", "One member of a pool.", None, &[("200", Some("PoolServersResponse"))], "read"),
            "put": operation(
                "putPoolServer",
                "Register a server or renew its registration; `?takeover=true` is admin only.",
                Some("RegistrationRequest"),
                &[("200", Some("PutResponse")), ("201", Some("PutResponse"))],
                "register",
            ),
            "delete": operation("deletePoolServer", "Remove a registration at once.", None, &[("204", None)], "register"),
        },
        "/api/v1/pools/{pool}/servers/{name}/heartbeat": {
            "parameters": [path_parameter("pool"), path_parameter("name")],
            "post": operation(
                "heartbeatPoolServer",
                "Renew a lease, optionally announcing a state change; the body may be empty.",
                Some("HeartbeatRequest"),
                &[("200", Some("HeartbeatResponse"))],
                "register",
            ),
        },
        "/api/v1/pools/{pool}/servers/{name}/state": {
            "parameters": [path_parameter("pool"), path_parameter("name")],
            "post": operation(
                "setPoolServerState",
                "Record the operator's drain or maintenance decision.",
                Some("OperatorStateRequest"),
                &[("204", None)],
                "admin",
            ),
        },
    });

    canonicalize(json!({
        "openapi": "3.1.0",
        "info": {
            "title": "OxiRoute Server Registry API",
            "version": "6.2.0",
            "description": "Lease-based dynamic server registration. The same routes are served by the management API (root token), by the registry TCP endpoint (scoped tokens) and by the registry unix sockets (access is the file mode, so no token). Every other path returns 404 on the registry endpoints."
        },
        "paths": paths,
        "components": {
            "schemas": schemas,
            "securitySchemes": {
                "registryBearer": { "type": "http", "scheme": "bearer" }
            }
        }
    }))
}

fn path_parameter(name: &str) -> Value {
    json!({ "name": name, "in": "path", "required": true, "schema": { "type": "string" } })
}

fn operation(
    id: &str,
    summary: &str,
    request: Option<&str>,
    successes: &[(&str, Option<&str>)],
    scope: &str,
) -> Value {
    let mut responses = Map::new();
    for (status, schema) in successes {
        let mut response = json!({ "description": "Success" });
        if let Some(schema) = schema {
            response["content"] = json!({ "application/json": { "schema": schema_ref(schema) } });
        }
        responses.insert((*status).to_owned(), response);
    }
    for (status, description) in [
        ("400", "The request is invalid."),
        ("401", "A valid bearer token is required."),
        (
            "403",
            "The credential does not cover this pool, server or operation.",
        ),
        ("404", "No such pool, registration or route."),
        ("409", "The name, address or lease conflicts with another."),
        ("413", "The request body exceeds 4 KiB."),
        ("429", "Rate limited; honour Retry-After."),
        ("503", "The registry is unavailable."),
    ] {
        responses.insert(
            status.to_owned(),
            json!({
                "description": description,
                "content": { "application/json": { "schema": schema_ref("ErrorBody") } }
            }),
        );
    }
    let mut operation = json!({
        "operationId": id,
        "summary": summary,
        "tags": ["Registry"],
        "security": [{ "registryBearer": [] }],
        "x-required-scope": scope,
        "responses": responses,
    });
    if let Some(request) = request {
        operation["requestBody"] = json!({
            "required": request != "HeartbeatRequest",
            "content": { "application/json": { "schema": schema_ref(request) } }
        });
    }
    operation
}

fn schema_ref(name: &str) -> Value {
    json!({ "$ref": format!("#/components/schemas/{name}") })
}

fn canonicalize(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize).collect()),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, canonicalize(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        value => value,
    }
}

fn artifact_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(ARTIFACT_RELATIVE_PATH)
}

fn generated_bytes() -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(&generated_openapi()).expect("OpenAPI serializes");
    bytes.push(b'\n');
    bytes
}

#[test]
fn the_checked_artifact_matches_the_handlers_types() {
    assert_eq!(
        fs::read(artifact_path()).expect("checked registry OpenAPI artifact"),
        generated_bytes()
    );
}

#[test]
fn every_registry_route_is_documented() {
    let document = generated_openapi();
    let paths = document["paths"].as_object().expect("paths");
    assert_eq!(paths.len(), 5);
    assert!(paths[SERVER].get("put").is_some() && paths[SERVER].get("delete").is_some());
}

#[test]
#[ignore = "run explicitly when regenerating the checked registry contract"]
fn write_checked_artifact() {
    fs::write(artifact_path(), generated_bytes()).expect("write registry OpenAPI artifact");
}
