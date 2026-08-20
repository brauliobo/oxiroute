use std::{collections::BTreeMap, fs, path::PathBuf};

use schemars::generate::SchemaSettings;
use serde_json::{Map, Value, json};

use super::{
    dto::{
        ErrorResponse, GenerationResponse, ListenerInventoryResponse, PoolInventoryResponse,
        ServerInventoryResponse, StatusResponse,
    },
    endpoint_registry::{self, EndpointSpec, SuccessSchema},
};

const ARTIFACT_RELATIVE_PATH: &str = "../../contracts/control-plane.openapi.json";

pub(super) fn generated_openapi() -> Value {
    let mut settings = SchemaSettings::draft2020_12().for_serialize();
    settings.definitions_path = "/components/schemas".into();
    settings.meta_schema = None;
    let mut generator = settings.into_generator();

    for endpoint in endpoint_registry::all() {
        register_success_schema(&mut generator, endpoint.success_schema);
    }
    generator.subschema_for::<ErrorResponse>();

    let mut schemas = Map::new();
    for (name, schema) in generator.take_definitions(false) {
        schemas.insert(name, schema);
    }

    let mut paths = Map::new();
    for endpoint in endpoint_registry::all() {
        paths.insert(
            endpoint.path.to_owned(),
            json!({ endpoint.method.to_ascii_lowercase(): operation(endpoint) }),
        );
    }

    canonicalize(json!({
        "openapi": "3.1.0",
        "info": {
            "title": "OxiRoute Control Plane API",
            "version": "0.5.2",
            "description": "Checked contract for the registry-owned protected read-only management endpoints."
        },
        "paths": paths,
        "components": {
            "schemas": schemas,
            "securitySchemes": {
                "managementBearer": {
                    "type": "http",
                    "scheme": "bearer"
                }
            }
        }
    }))
}

fn register_success_schema(generator: &mut schemars::SchemaGenerator, schema: SuccessSchema) {
    match schema {
        SuccessSchema::Status => {
            generator.subschema_for::<StatusResponse>();
        }
        SuccessSchema::Listeners => {
            generator.subschema_for::<ListenerInventoryResponse>();
        }
        SuccessSchema::Pools => {
            generator.subschema_for::<PoolInventoryResponse>();
        }
        SuccessSchema::Servers => {
            generator.subschema_for::<ServerInventoryResponse>();
        }
        SuccessSchema::Generations => {
            generator.subschema_for::<GenerationResponse>();
        }
    }
}

fn operation(endpoint: &EndpointSpec) -> Value {
    debug_assert_eq!(endpoint.method, "GET");
    debug_assert_eq!(
        endpoint.response,
        super::endpoint_registry::ResponseMode::Json
    );
    debug_assert_eq!(
        endpoint.auth,
        super::endpoint_registry::AuthPolicy::ManagementBearer
    );
    json!({
        "operationId": endpoint.operation_id,
        "tags": ["Management"],
        "security": [{ "managementBearer": [] }],
        "responses": {
            "200": {
                "description": "Successful response",
                "headers": {
                    "X-Correlation-Id": {
                        "description": "Request correlation identifier.",
                        "schema": { "type": "string" }
                    }
                },
                "content": {
                    "application/json": {
                        "schema": schema_ref(success_schema_name(endpoint.success_schema))
                    }
                }
            },
            "401": error_response("Authentication is required.", true),
            "405": error_response("The HTTP method is not allowed.", false)
        }
    })
}

fn error_response(description: &str, authenticate: bool) -> Value {
    let mut headers = Map::new();
    headers.insert(
        "X-Correlation-Id".into(),
        json!({
            "description": "Request correlation identifier.",
            "schema": { "type": "string" }
        }),
    );
    if authenticate {
        headers.insert(
            "WWW-Authenticate".into(),
            json!({
                "description": "Bearer authentication challenge.",
                "schema": { "type": "string" }
            }),
        );
    } else {
        headers.insert(
            "Allow".into(),
            json!({
                "description": "Allowed HTTP methods.",
                "schema": { "type": "string" }
            }),
        );
    }
    json!({
        "description": description,
        "headers": headers,
        "content": {
            "application/json": {
                "schema": schema_ref("ErrorResponse")
            }
        }
    })
}

fn schema_ref(name: &str) -> Value {
    json!({ "$ref": format!("#/components/schemas/{name}") })
}

fn success_schema_name(schema: SuccessSchema) -> &'static str {
    match schema {
        SuccessSchema::Status => "StatusResponse",
        SuccessSchema::Listeners => "ListenerInventoryResponse",
        SuccessSchema::Pools => "PoolInventoryResponse",
        SuccessSchema::Servers => "ServerInventoryResponse",
        SuccessSchema::Generations => "GenerationResponse",
    }
}

fn canonicalize(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize).collect()),
        Value::Object(values) => {
            let values = values
                .into_iter()
                .map(|(key, value)| (key, canonicalize(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(values.into_iter().collect())
        }
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
    fn generated_operations_match_the_endpoint_registry() {
        let document = generated_openapi();
        let paths = document["paths"].as_object().expect("OpenAPI paths");
        assert_eq!(paths.len(), endpoint_registry::all().len());
        for endpoint in endpoint_registry::all() {
            let operation = &paths[endpoint.path][endpoint.method.to_ascii_lowercase()];
            assert_eq!(operation["operationId"], endpoint.operation_id);
            assert_eq!(operation["security"][0]["managementBearer"], json!([]));
            assert_eq!(
                operation["responses"]["200"]["content"]["application/json"]["schema"],
                schema_ref(success_schema_name(endpoint.success_schema))
            );
        }
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
