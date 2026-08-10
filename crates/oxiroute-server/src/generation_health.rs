use serde_json::{Value, json};

pub(crate) fn generation_component_status(generation: Option<&crate::GenerationStatus>) -> Value {
    match generation {
        Some(status) if status.degraded => json!({
            "state": "degraded",
            "reason": status.last_failure,
        }),
        Some(status) if status.active_revision.is_some() => json!({ "state": "healthy" }),
        Some(_) | None => json!({
            "state": "degraded",
            "reason": "active_generation_unavailable",
        }),
    }
}
