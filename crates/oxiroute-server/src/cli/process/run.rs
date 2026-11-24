use serde_json::json;

use super::ProcessCommand;
use crate::cli::{
    context::{Context, Execute},
    error::CliError,
    net::{Client, Response},
    overlay,
    view::View,
    wait::{self, Attempt},
};

fn ready(context: &Context<'_>, wait: bool) -> Result<(), CliError> {
    let client = context.public_client()?;
    let response = if wait {
        wait::until("readiness", context.timeouts()?, || probe(&client))?
    } else {
        client.get_unchecked("/ready")?
    };
    context.print_response(&response, View::Generic)?;
    if response.is_success() {
        Ok(())
    } else {
        Err(CliError::Status(response.status, "not ready".into()))
    }
}

fn probe(client: &Client) -> Result<Attempt<Response>, CliError> {
    match client.get_unchecked("/ready") {
        Ok(response) if response.is_success() => Ok(Attempt::Done(response)),
        Ok(response) => Ok(Attempt::Pending(format!(
            "the daemon answered HTTP {}",
            response.status
        ))),
        Err(error @ (CliError::Connect(_) | CliError::Timeout(_))) => {
            Ok(Attempt::Pending(error.to_string()))
        }
        Err(error) => Err(error),
    }
}

impl Execute for ProcessCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        match self {
            Self::Status => context
                .show("/api/v1/status", View::Status)
                .and_then(|()| overlay::announce(context)),
            Self::Ready { wait } => ready(context, *wait),
            Self::Metrics => context.show_text("/metrics"),
            Self::Topology => context.show("/api/v1/topology", View::Topology),
            Self::Monitoring => context.show("/api/v1/monitoring", View::Monitoring),
            Self::Reload { wait } => crate::cli::generation::reload(context, *wait),
            Self::Drain => context.mutate("POST", "/api/v1/process/drain", &json!({})),
            Self::Shutdown => context.mutate("POST", "/api/v1/process/shutdown", &json!({})),
        }
    }
}
