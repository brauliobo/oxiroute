use schemars::JsonSchema;
use serde::{Serialize, Serializer};

#[derive(JsonSchema, Serialize)]
pub(crate) struct ErrorResponse {
    #[schemars(with = "ErrorBodySchema")]
    error: ErrorBody,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
#[schemars(rename = "ErrorBody")]
struct ErrorBodySchema {
    code: String,
    message: String,
}

impl ErrorResponse {
    pub(crate) fn new(code: &'static str, message: String) -> Self {
        Self {
            error: ErrorBody { code, message },
        }
    }
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

#[derive(Clone, Copy, Debug, Eq, JsonSchema, PartialEq)]
#[schemars(transparent)]
pub(crate) struct DecimalCounter(#[schemars(with = "String", regex(pattern = "^[0-9]+$"))] u64);

impl From<u64> for DecimalCounter {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl Serialize for DecimalCounter {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

#[cfg(test)]
mod tests {
    use schemars::generate::SchemaSettings;
    use serde_json::json;

    use super::{DecimalCounter, ErrorResponse};

    #[test]
    fn common_response_schemas_match_the_version_one_contract() {
        let generator = SchemaSettings::default().for_serialize().into_generator();
        let error = serde_json::to_value(generator.into_root_schema_for::<ErrorResponse>())
            .expect("error response schema");

        assert_eq!(
            error,
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "$defs": {
                    "ErrorBody": {
                        "type": "object",
                        "properties": {
                            "code": { "type": "string" },
                            "message": { "type": "string" },
                        },
                        "required": ["code", "message"],
                    },
                },
                "title": "ErrorResponse",
                "type": "object",
                "properties": {
                    "error": { "$ref": "#/$defs/ErrorBody" },
                },
                "required": ["error"],
            })
        );
    }

    #[test]
    fn decimal_counter_schema_is_an_unsigned_decimal_string() {
        let schema = serde_json::to_value(schemars::schema_for!(DecimalCounter))
            .expect("decimal counter schema");

        assert_eq!(
            schema,
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "title": "DecimalCounter",
                "type": "string",
                "pattern": "^[0-9]+$",
            })
        );
    }

    #[test]
    fn decimal_counter_preserves_the_maximum_u64() {
        assert_eq!(
            serde_json::to_string(&DecimalCounter::from(u64::MAX)).expect("counter JSON"),
            r#""18446744073709551615""#
        );
    }
}
