use serde_json::Value;

use super::{
    args::ApplyArgs,
    daemon::{self, Checked, Stored},
    input::{InputFormat, SourceInput},
    report,
};
use crate::cli::{
    context::Context,
    error::CliError,
    generation::{Baseline, Settled, settle},
    net::Client,
    render,
};

pub(super) fn apply(context: &Context<'_>, args: &ApplyArgs) -> Result<(), CliError> {
    let client = context.client()?;
    let (stored, _) = Stored::read(&client, "disk")?;
    let input = SourceInput::read(&args.source.file, args.source.format)?;
    let input = fit(input, &stored, args.convert)?;
    if !args.dry_run {
        stored.ensure_writable()?;
    }
    let label = args.source.file.display().to_string();
    match daemon::validate(&client, &input.request(false))? {
        Checked::Invalid(response, body) => Err(daemon::refusal(context, &label, &response, &body)),
        Checked::Valid(response, body) if args.dry_run => {
            if !context.quiet() {
                render::diagnose("dry run: the configuration is valid and nothing was written");
            }
            daemon::print(context, &response, &report::validation(&body))
        }
        Checked::Valid(..) => {
            let baseline = baseline(&client, args.wait)?;
            let saved = store(context, &client, &stored, &input, &label)?;
            finish(context, &client, &saved, baseline.as_ref())
        }
    }
}

fn fit(input: SourceInput, stored: &Stored, convert: bool) -> Result<SourceInput, CliError> {
    if input.matches(stored.format) {
        return Ok(input);
    }
    if convert || input.format == InputFormat::Json {
        return input.converted(stored.format);
    }
    let format = stored.format.wire();
    Err(CliError::Local(format!(
        "{} is {} but the daemon runs a {format} file: pass --convert to store the equivalent \
         {format} text (the comments and logic of the file are not kept), or convert it with \
         `oxiroute config compose --format {format}`",
        input.path.display(),
        input.format.wire(),
    )))
}

pub(super) fn baseline(client: &Client, wait: bool) -> Result<Option<Baseline>, CliError> {
    wait.then(|| Baseline::of(client)).transpose()
}

pub(super) fn store(
    context: &Context<'_>,
    client: &Client,
    stored: &Stored,
    input: &SourceInput,
    label: &str,
) -> Result<Value, CliError> {
    match daemon::save(client, &input.request(false), &stored.disk_revision)? {
        Checked::Invalid(response, body) => Err(daemon::refusal(context, label, &response, &body)),
        Checked::Valid(response, body) => {
            daemon::print(context, &response, &report::saved(&body))?;
            if body.get("written") == Some(&Value::Bool(false)) && !context.quiet() {
                render::diagnose(
                    "the file is identical to the stored configuration: nothing was written",
                );
            }
            Ok(body)
        }
    }
}

pub(super) fn finish(
    context: &Context<'_>,
    client: &Client,
    saved: &Value,
    baseline: Option<&Baseline>,
) -> Result<(), CliError> {
    if saved.get("outcome").and_then(Value::as_str) != Some("saved_pending_activation") {
        return Ok(());
    }
    let Some(baseline) = baseline else {
        if !context.quiet() {
            render::diagnose(
                "saved; the daemon applies it on its next reconcile (add --wait to wait for it)",
            );
        }
        return Ok(());
    };
    if !context.quiet() {
        render::diagnose("waiting for the saved revision to become active");
    }
    match settle(
        client,
        context.timeouts()?,
        "the configuration",
        saved.get("candidateRevision"),
        baseline,
    )? {
        Settled::Activated(_) => Ok(()),
        Settled::Rejected(summary) => Err(CliError::Remote(format!(
            "the daemon rejected the saved configuration: {summary}"
        ))),
    }
}
