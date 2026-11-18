use serde_json::json;

use super::GenerationCommand;
use crate::cli::{
    context::{Context, Execute},
    error::CliError,
    view::View,
};

impl Execute for GenerationCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        match self {
            Self::Status => context.show("/api/v1/generations", View::Generation),
            Self::Reload => context.mutate("POST", "/api/v1/generations/reload", &json!({})),
            Self::Rollback => context.mutate("POST", "/api/v1/generations/rollback", &json!({})),
            Self::Drain { timeout_ms } => context.mutate(
                "POST",
                "/api/v1/generations/drain",
                &json!({ "timeoutMs": timeout_ms }),
            ),
        }
    }
}
