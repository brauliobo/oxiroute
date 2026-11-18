use std::time::Duration;

use serde_json::json;

use super::{GenerationCommand, reload::reload};
use crate::cli::{
    context::{Context, Execute},
    error::CliError,
    net::{Timeouts, span},
    view::View,
};

const MAX_DRAIN_WAIT: Duration = Duration::from_mins(5);
const ANSWER_SLACK: Duration = Duration::from_secs(5);

impl Execute for GenerationCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        match self {
            Self::Status => context.show("/api/v1/generations", View::Generation),
            Self::Reload { wait } => reload(context, *wait),
            Self::Rollback => context.mutate("POST", "/api/v1/generations/rollback", &json!({})),
            Self::Drain { wait } => drain(context, *wait),
        }
    }
}

fn drain(context: &Context<'_>, wait: bool) -> Result<(), CliError> {
    let limit = if wait {
        context.timeouts()?.wait()
    } else {
        Duration::ZERO
    };
    if limit > MAX_DRAIN_WAIT {
        return Err(CliError::Local(format!(
            "a drain waits at most {}; lower --timeout",
            span(MAX_DRAIN_WAIT)
        )));
    }
    let timeouts = if wait {
        Timeouts::limited(limit + ANSWER_SLACK)
    } else {
        context.timeouts()?
    };
    let body = json!({ "timeoutMs": u64::try_from(limit.as_millis()).unwrap_or(u64::MAX) });
    let response = context.client_within(timeouts)?.mutation_json(
        "POST",
        "/api/v1/generations/drain",
        &body,
    )?;
    context.print_response(&response, View::Generic)
}
