//! The `OpenAPI` document of the page API, generated from the snapshot types and checked in as
//! `contracts/stats-page.openapi.json` so the frontend builds against a stable contract.

use std::{collections::BTreeMap, fs, path::PathBuf};

use schemars::{Schema, generate::SchemaSettings, transform::RecursiveTransform};
use serde_json::{Map, Value, json};

use schemars::JsonSchema;
use serde::Serialize;

use super::model::{AdministrativeState, SCHEMA_VERSION, StatsDocument};
use crate::rtmp::api::dto::ErrorResponse;

/// The administration form the page posts as `application/x-www-form-urlencoded`: the same fields,
/// checks and effect for the no-script table and for a script client.
#[derive(JsonSchema, Serialize)]
struct AdminForm {
    /// `generation.activeRevision` of the snapshot the form was built from; a changed revision is a 409.
    generation_revision: String,
    pool: String,
    server: String,
    state: AdministrativeState,
}

const ARTIFACT_RELATIVE_PATH: &str = "../../contracts/stats-page.openapi.json";

pub(super) fn generated_openapi() -> Value {
    let mut settings = SchemaSettings::draft2020_12().for_serialize();
    settings.definitions_path = "/components/schemas".into();
    settings.meta_schema = None;
    settings
        .transforms
        .push(Box::new(RecursiveTransform(omit_instead_of_null)));
    let mut generator = settings.into_generator();
    generator.subschema_for::<StatsDocument>();
    generator.subschema_for::<AdminForm>();
    generator.subschema_for::<ErrorResponse>();
    let schemas: Map<String, Value> = generator.take_definitions(true).into_iter().collect();

    canonicalize(json!({
        "openapi": "3.1.0",
        "info": {
            "title": "OxiRoute Stats Page API",
            "version": SCHEMA_VERSION.to_string(),
            "description": "Read-only statistics served by a stats page listener. Every client that can reach the listener gets the same snapshot; only `admin` depends on the peer. Keys the proxy does not know are omitted, never zero. Counters are decimal strings, gauges and timestamps are numbers. All responses are `Cache-Control: no-store`; `HEAD` answers like `GET` without a body."
        },
        "servers": [{
            "url": "http://{host}{prefix}",
            "variables": {
                "host": { "default": "127.0.0.1:8404" },
                "prefix": {
                    "default": "/stats",
                    "description": "The `uri_prefix` of the stats page; `/` when it is the root."
                }
            }
        }],
        "paths": {
            "/snapshot.json": { "get": snapshot_operation() },
            "/stats.csv": { "get": csv_operation() },
            "/": { "get": page_operation(), "post": admin_operation() }
        },
        "components": { "schemas": schemas }
    }))
}

/// An `Option` field of the snapshot is left out when it is `None`; it is never `null`.
fn omit_instead_of_null(schema: &mut Schema) {
    let Some(Value::Object(properties)) = schema.get_mut("properties") else {
        return;
    };
    for property in properties.values_mut() {
        if let Some(Value::Array(types)) = property.get_mut("type") {
            types.retain(|name| name != "null");
            if let [only] = types.as_slice() {
                *property.get_mut("type").expect("type") = only.clone();
            }
        }
        if let Some(Value::Array(options)) = property.get_mut("anyOf") {
            options.retain(|option| option != &json!({ "type": "null" }));
            if let [only] = options.as_slice() {
                *property = only.clone();
            }
        }
    }
}

fn snapshot_operation() -> Value {
    json!({
        "operationId": "getStatsSnapshot",
        "summary": "The complete snapshot.",
        "description": "Rebuilt at most once per second and shared by every client. Poll no faster than `intervalHintMs`.",
        "responses": {
            "200": json_body("The snapshot.", "StatsDocument"),
            "503": error_response("No generation is active or the runtime could not be sampled.")
        }
    })
}

fn csv_operation() -> Value {
    json!({
        "operationId": "getStatsCsv",
        "summary": "HAProxy-style CSV of listeners (`FRONTEND`), pools (`BACKEND`) and servers.",
        "description": "One header line starting with `# `, then one row per listener, pool and server, under the HAProxy column names for the values the proxy has. Unknown cells are empty. Text cells are quoted when needed and prefixed with `'` when they start with a formula character.",
        "responses": {
            "200": {
                "description": "The CSV.",
                "content": { "text/csv": { "schema": { "type": "string" } } }
            },
            "503": error_response("No generation is active or the runtime could not be sampled.")
        }
    })
}

fn page_operation() -> Value {
    json!({
        "operationId": "getStatsPage",
        "summary": "The page: a table that needs no script.",
        "responses": {
            "200": {
                "description": "The HTML page.",
                "content": { "text/html": { "schema": { "type": "string" } } }
            },
            "503": error_response("No generation is active or the runtime could not be sampled.")
        }
    })
}

fn admin_operation() -> Value {
    json!({
        "operationId": "changeServerState",
        "summary": "Changes the administrative state of one server.",
        "description": "Only for loopback peers of a page configured with `admin = \"localhost\"` and only when the request is same-origin (loopback `Host`, matching `Origin` or `Referer`, no forwarding headers); `admin` in the snapshot says whether to offer it.",
        "requestBody": {
            "required": true,
            "content": {
                "application/x-www-form-urlencoded": { "schema": schema_ref("AdminForm") }
            }
        },
        "responses": {
            "204": { "description": "The state changed; the next snapshot shows it." },
            "400": error_response("The form is invalid."),
            "403": error_response("The peer or the request origin may not administer."),
            "404": error_response("The pool or the server does not exist."),
            "409": error_response("The generation revision changed since the snapshot."),
            "413": error_response("The form exceeds 8 KiB."),
            "415": error_response("The content type is not `application/x-www-form-urlencoded`.")
        }
    })
}

fn json_body(description: &str, schema: &str) -> Value {
    json!({
        "description": description,
        "content": { "application/json": { "schema": schema_ref(schema) } }
    })
}

fn error_response(description: &str) -> Value {
    json_body(description, "ErrorResponse")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_artifact_matches_generated_openapi() {
        let actual = fs::read(artifact_path()).expect("checked OpenAPI artifact");
        assert_eq!(actual, generated_bytes());
    }

    #[test]
    fn the_contract_describes_exactly_the_page_routes() {
        let document = generated_openapi();
        let paths = document["paths"].as_object().expect("paths");
        assert_eq!(
            paths.keys().collect::<Vec<_>>(),
            ["/", "/snapshot.json", "/stats.csv"]
        );
        assert_eq!(
            paths["/"].as_object().unwrap().keys().collect::<Vec<_>>(),
            ["get", "post"]
        );
        assert_eq!(document["info"]["version"], "1");
        assert!(
            document.get("security").is_none(),
            "the page is open to every client that can reach it"
        );
    }

    #[test]
    #[ignore = "run explicitly when regenerating the checked contract artifact"]
    fn write_checked_artifact() {
        let path = artifact_path();
        fs::create_dir_all(path.parent().expect("artifact directory"))
            .expect("create contract directory");
        fs::write(path, generated_bytes()).expect("write checked OpenAPI artifact");
    }
}
