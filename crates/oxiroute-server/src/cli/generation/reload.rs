use serde_json::json;

use super::settle::{Baseline, Settled, settle};
use crate::cli::{context::Context, error::CliError, render, view::View};

const RELOAD: &str = "/api/v1/generations/reload";

pub(super) fn reload(context: &Context<'_>, wait: bool) -> Result<(), CliError> {
    if !wait {
        return context.mutate("POST", RELOAD, &json!({}));
    }
    let client = context.client()?;
    let baseline = Baseline::of(&client)?;
    let requested = client.mutation_json("POST", RELOAD, &json!({}))?.json()?;
    let candidate = requested.get("candidateRevision").cloned();
    if !context.quiet() {
        render::diagnose(
            "oxiroute: reload requested, waiting for the candidate revision to become active",
        );
    }
    match settle(
        &client,
        context.timeouts()?,
        "the reload",
        candidate.as_ref(),
        &baseline,
    )? {
        Settled::Activated(response) => context.print_response(&response, View::Generation),
        Settled::Rejected(summary) => Err(CliError::Remote(format!("reload rejected: {summary}"))),
    }
}
