use serde_json::Value;

use super::{
    args::{Against, DiffArgs, SourceArgs},
    daemon::{self, Checked},
    history,
    input::SourceInput,
    report,
};
use crate::cli::{context::Context, error::CliError, global::Output, render};

pub(super) fn validate(context: &Context<'_>, args: &SourceArgs) -> Result<(), CliError> {
    let client = context.client()?;
    let input = SourceInput::read(&args.file, args.format)?;
    match daemon::validate(&client, &input.request(false))? {
        Checked::Valid(response, body) => {
            daemon::print(context, &response, &report::validation(&body))
        }
        Checked::Invalid(response, body) => Err(daemon::refusal(
            context,
            &args.file.display().to_string(),
            &response,
            &body,
        )),
    }
}

pub(super) fn diff(context: &Context<'_>, args: &DiffArgs) -> Result<(), CliError> {
    let Some(file) = &args.file else {
        return history::diff(context, args);
    };
    let client = context.client()?;
    let input = SourceInput::read(file, args.format)?;
    let (response, body) = match daemon::validate(&client, &input.request(args.text))? {
        Checked::Valid(response, body) => (response, body),
        Checked::Invalid(response, body) => {
            return Err(daemon::refusal(
                context,
                &file.display().to_string(),
                &response,
                &body,
            ));
        }
    };
    let against = match args.against {
        Against::Active => "active",
        Against::Disk => "disk",
    };
    let comparison = body
        .pointer(&format!("/diff/{against}"))
        .filter(|comparison| !comparison.is_null())
        .ok_or_else(|| {
            CliError::Local(format!(
                "there is no {against} configuration to compare with"
            ))
        })?;
    let text_diff = body
        .get("textDiff")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty());
    if matches!(context.output(), Output::Json) {
        render::print_json_body(&response, context.quiet())?;
    } else if !context.quiet() {
        print_changes(
            comparison,
            against,
            args.text,
            body.get("textDiff"),
            text_diff,
        )?;
    }
    let differs = !report::is_equal(comparison) || (args.text && text_diff.is_some());
    if args.exit_code && differs {
        return Err(CliError::Differs);
    }
    Ok(())
}

fn print_changes(
    comparison: &Value,
    against: &str,
    text: bool,
    text_diff: Option<&Value>,
    unified: Option<&str>,
) -> Result<(), CliError> {
    if report::is_equal(comparison) {
        render::diagnose(&format!(
            "no differences against the {against} configuration"
        ));
    } else {
        render::emit(&report::changes(comparison))?;
    }
    if !text {
        return Ok(());
    }
    match (unified, text_diff) {
        (Some(unified), _) => render::emit(unified),
        (None, Some(Value::String(_))) => {
            render::diagnose("the text is identical to the stored file");
            Ok(())
        }
        (None, _) => {
            render::diagnose("no text diff: the file is not in the format of the daemon's file");
            Ok(())
        }
    }
}
