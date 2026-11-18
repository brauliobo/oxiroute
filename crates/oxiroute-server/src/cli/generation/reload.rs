use serde_json::{Value, json};

use crate::cli::{
    context::Context,
    error::CliError,
    net::Response,
    render,
    view::{self, View},
    wait::{self, Attempt},
};

const STATUS: &str = "/api/v1/generations";
const RELOAD: &str = "/api/v1/generations/reload";

enum Settled {
    Activated(Response),
    Rejected(String),
}

pub(super) fn reload(context: &Context<'_>, wait: bool) -> Result<(), CliError> {
    if !wait {
        return context.mutate("POST", RELOAD, &json!({}));
    }
    let client = context.client()?;
    let before = client.get(STATUS, true)?.json()?;
    let rejection = before.pointer("/generation/lastRejection").cloned();
    let requested = client.mutation_json("POST", RELOAD, &json!({}))?.json()?;
    let candidate = requested.get("candidateRevision").cloned();
    if !context.quiet() {
        render::diagnose(
            "oxiroute: reload requested, waiting for the candidate revision to become active",
        );
    }
    let settled = wait::until("the reload", context.timeouts(), || {
        let response = client.get(STATUS, true)?;
        let status = response.json()?;
        let generation = status.get("generation").unwrap_or(&Value::Null);
        if generation.get("activeRevision") == candidate.as_ref() {
            return Ok(Attempt::Done(Settled::Activated(response)));
        }
        let latest = generation
            .get("lastRejection")
            .filter(|value| value.is_object());
        if latest.is_some() && latest != rejection.as_ref() {
            let summary = view::rejection_summary(generation).unwrap_or_else(|| "rejected".into());
            return Ok(Attempt::Done(Settled::Rejected(summary)));
        }
        Ok(Attempt::Pending(
            "the candidate revision is not active yet".into(),
        ))
    })?;
    match settled {
        Settled::Activated(response) => context.print_response(&response, View::Generation),
        Settled::Rejected(summary) => Err(CliError::Remote(format!("reload rejected: {summary}"))),
    }
}
