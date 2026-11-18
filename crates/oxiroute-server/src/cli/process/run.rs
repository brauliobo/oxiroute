use serde_json::json;

use super::ProcessCommand;
use crate::cli::{
    context::{Context, Execute},
    error::CliError,
};

fn ready(context: &Context<'_>) -> Result<(), CliError> {
    let response = context.public_client()?.get_unchecked("/ready")?;
    context.print_response(&response)?;
    if response.is_success() {
        Ok(())
    } else {
        Err(CliError::Status(response.status, "not ready".into()))
    }
}

impl Execute for ProcessCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        match self {
            Self::Status => context.show("/api/v1/status"),
            Self::Ready => ready(context),
            Self::Metrics => context.show_public("/metrics"),
            Self::Topology => context.show("/api/v1/topology"),
            Self::Monitoring => context.show("/api/v1/monitoring"),
            Self::Drain => context.mutate("POST", "/api/v1/process/drain", &json!({})),
            Self::Shutdown => context.mutate("POST", "/api/v1/process/shutdown", &json!({})),
        }
    }
}
