use std::path::Path;

use serde_json::{Value, json};

use super::{ConfigCommand, check, compose::compose};
use crate::cli::{
    context::{Context, Execute},
    error::CliError,
    fs::{MAX_FILE_BYTES, read_regular_file, unreadable_config},
    net::Client,
    render,
};

impl Execute for ConfigCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        match self {
            Self::Check { config } => {
                render::emit(&check::report(config)?);
                Ok(())
            }
            Self::Compose { format, configs } => {
                render::emit(&compose(configs, (*format).into())?);
                Ok(())
            }
            Self::Get => context.show("/api/v1/config"),
            Self::Validate { file } => validate(context, file),
            Self::Apply { file } => apply(context, file),
            Self::Diff { file } => diff(context, file),
        }
    }
}

fn validate(context: &Context<'_>, file: &Path) -> Result<(), CliError> {
    let response = context.client()?.json(
        "POST",
        "/api/v1/config/validate",
        &json!({ "config": config_file(file)? }),
        &[],
    )?;
    context.print_response(&response)
}

fn apply(context: &Context<'_>, file: &Path) -> Result<(), CliError> {
    let client = context.client()?;
    let current = current_config(&client)?;
    let revision = current
        .get("diskRevision")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::Local("configuration response has no disk revision".into()))?;
    let response = client.json(
        "PUT",
        "/api/v1/config",
        &json!({ "config": config_file(file)? }),
        &[("If-Config-Revision", revision)],
    )?;
    context.print_response(&response)
}

fn diff(context: &Context<'_>, file: &Path) -> Result<(), CliError> {
    let active = current_config(&context.client()?)?
        .get("config")
        .cloned()
        .ok_or_else(|| CliError::Local("configuration response has no config field".into()))?;
    let candidate = config_file(file)?;
    context.print(&json!({
        "different": active != candidate,
        "active": active,
        "candidate": candidate,
    }))
}

fn current_config(client: &Client) -> Result<Value, CliError> {
    client.get("/api/v1/config", true)?.json()
}

fn config_file(path: &Path) -> Result<Value, CliError> {
    let bytes = read_regular_file(path, MAX_FILE_BYTES, false)
        .map_err(|error| unreadable_config(path, &error))?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
        CliError::Local(format!(
            "configuration {} must contain JSON: {error}",
            path.display()
        ))
    })?;
    Ok(value.get("config").cloned().unwrap_or(value))
}
