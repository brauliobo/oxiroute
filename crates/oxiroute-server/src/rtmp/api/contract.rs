use std::{collections::BTreeMap, fs, path::PathBuf};

use schemars::generate::SchemaSettings;
use serde_json::{Map, Value, json};

use super::{
    dto::{
        ErrorResponse, GenerationResponse, ListenerInventoryResponse, MediaTicketRequest,
        PoolInventoryResponse, RequestListResponse, RtmpSecretsResponse, ServerInventoryResponse,
        StatusResponse, TicketResponse, VodTicketRequest,
    },
    endpoint_registry::{self, EndpointSpec, RequestSchema, SuccessSchema},
};

const ARTIFACT_RELATIVE_PATH: &str = "../../contracts/control-plane.openapi.json";

pub(super) fn generated_openapi() -> Value {
    let mut settings = SchemaSettings::draft2020_12().for_serialize();
    settings.definitions_path = "/components/schemas".into();
    settings.meta_schema = None;
    let mut generator = settings.into_generator();

    for endpoint in endpoint_registry::all() {
        register_success_schema(&mut generator, endpoint.success_schema);
        if let Some(request) = endpoint.request {
            register_request_schema(&mut generator, request.schema);
        }
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
            "version": "7.1.0",
            "description": "Checked contract for the registry-owned protected management endpoints."
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
        SuccessSchema::Requests => {
            generator.subschema_for::<RequestListResponse>();
        }
        SuccessSchema::Ticket => {
            generator.subschema_for::<TicketResponse>();
        }
        SuccessSchema::RtmpSecrets => {
            generator.subschema_for::<RtmpSecretsResponse>();
        }
    }
}

fn register_request_schema(generator: &mut schemars::SchemaGenerator, schema: RequestSchema) {
    match schema {
        RequestSchema::MediaTicket => {
            generator.subschema_for::<MediaTicketRequest>();
        }
        RequestSchema::VodTicket => {
            generator.subschema_for::<VodTicketRequest>();
        }
    }
}

fn operation(endpoint: &EndpointSpec) -> Value {
    debug_assert_eq!(
        endpoint.response,
        super::endpoint_registry::ResponseMode::Json
    );
    debug_assert_eq!(
        endpoint.auth,
        super::endpoint_registry::AuthPolicy::ManagementBearer
    );
    let mut responses = Map::new();
    responses.insert(
        "200".into(),
        json!({
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
        }),
    );
    for failure in endpoint.failures {
        responses.insert(
            failure.status.into(),
            error_response(failure.description, false, false),
        );
    }
    responses.insert(
        "401".into(),
        error_response("Authentication is required.", true, false),
    );
    responses.insert(
        "405".into(),
        error_response("The HTTP method is not allowed.", false, true),
    );
    let mut operation = Map::new();
    operation.insert("operationId".into(), json!(endpoint.operation_id));
    operation.insert("tags".into(), json!(["Management"]));
    operation.insert("security".into(), json!([{ "managementBearer": [] }]));
    let parameters = endpoint
        .path
        .split('/')
        .filter_map(|segment| segment.strip_prefix('{')?.strip_suffix('}'))
        .map(path_parameter)
        .collect::<Vec<_>>();
    if !parameters.is_empty() {
        operation.insert("parameters".into(), Value::Array(parameters));
    }
    if let Some(request) = endpoint.request {
        operation.insert(
            "requestBody".into(),
            json!({
                "required": request.required,
                "content": {
                    "application/json": {
                        "schema": schema_ref(request_schema_name(request.schema))
                    }
                }
            }),
        );
    }
    operation.insert("responses".into(), Value::Object(responses));
    Value::Object(operation)
}

fn path_parameter(name: &str) -> Value {
    json!({
        "name": name,
        "in": "path",
        "required": true,
        "schema": { "type": "string", "minLength": 1 }
    })
}

fn error_response(description: &str, authenticate: bool, allow: bool) -> Value {
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
    }
    if allow {
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
        SuccessSchema::Requests => "RequestListResponse",
        SuccessSchema::Ticket => "TicketResponse",
        SuccessSchema::RtmpSecrets => "RtmpSecretsResponse",
    }
}

fn request_schema_name(schema: RequestSchema) -> &'static str {
    match schema {
        RequestSchema::MediaTicket => "MediaTicketRequest",
        RequestSchema::VodTicket => "VodTicketRequest",
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
