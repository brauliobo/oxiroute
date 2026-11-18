use serde_json::json;

use super::ProcessCommand;
use crate::cli::{
    context::{Context, Execute},
    error::CliError,
    view::View,
};

fn ready(context: &Context<'_>) -> Result<(), CliError> {
    let response = context.public_client()?.get_unchecked("/ready")?;
    context.print_response(&response, View::Generic)?;
    if response.is_success() {
        Ok(())
    } else {
        Err(CliError::Status(response.status, "not ready".into()))
    }
}

impl Execute for ProcessCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        match self {
            Self::Status => context.show("/api/v1/status", View::Status),
            Self::Ready => ready(context),
            Self::Metrics => context.show_text("/metrics"),
            Self::Topology => context.show("/api/v1/topology", View::Topology),
            Self::Monitoring => context.show("/api/v1/monitoring", View::Monitoring),
            Self::Drain => context.mutate("POST", "/api/v1/process/drain", &json!({})),
            Self::Shutdown => context.mutate("POST", "/api/v1/process/shutdown", &json!({})),
        }
    }
}
