use serde_json::Value;

use super::{input::InputFormat, report};
use crate::cli::{
    context::Context,
    error::CliError,
    global::Output,
    net::{Client, Response},
    render::{self, Document},
};

const SOURCE: &str = "/api/v1/config/source";
const VALIDATE: &str = "/api/v1/config/source/validate";

/// The configuration file the daemon runs, as `GET /config/source` describes it.
pub(super) struct Stored {
    pub(super) body: Value,
    pub(super) format: InputFormat,
    pub(super) disk_revision: String,
    pub(super) text: Option<String>,
}

impl Stored {
    pub(super) fn read(client: &Client, view: &str) -> Result<(Self, Response), CliError> {
        let response = client.get(&format!("{SOURCE}?view={view}"), true)?;
        let body = response.json()?;
        let field = |name: &str| {
            body.get(name)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| CliError::Remote(format!("configuration source has no {name}")))
        };
        let format = field("format")?;
        let stored = Self {
            format: InputFormat::named(&format).ok_or_else(|| {
                CliError::Remote(format!("unknown configuration format {format}"))
            })?,
            disk_revision: field("diskRevision")?,
            text: body.get("text").and_then(Value::as_str).map(str::to_owned),
            body,
        };
        Ok((stored, response))
    }

    pub(super) fn ensure_writable(&self) -> Result<(), CliError> {
        let block = self.body.get("writeBlock").and_then(Value::as_str);
        match block {
            None => Ok(()),
            Some(block) => Err(CliError::Remote(report::unwritable(block))),
        }
    }

    pub(super) fn readable(&self) -> Result<&str, CliError> {
        self.text.as_deref().ok_or_else(|| {
            CliError::Remote(
                "the daemon withheld the configuration text because it holds secrets that cannot \
                 be redacted reliably; read the file on the host instead"
                    .into(),
            )
        })
    }
}

pub(super) enum Checked {
    Valid(Response, Value),
    Invalid(Response, Value),
}

pub(super) fn validate(client: &Client, request: &Value) -> Result<Checked, CliError> {
    answer(client.json_unchecked("POST", VALIDATE, request, &[])?)
}

pub(super) fn save(client: &Client, request: &Value, revision: &str) -> Result<Checked, CliError> {
    let response =
        client.json_unchecked("PUT", SOURCE, request, &[("If-Config-Revision", revision)])?;
    match response.status {
        409 => Err(CliError::Status(
            409,
            "config_changed: the configuration file changed since it was read; run the command \
             again to start from the new revision"
                .into(),
        )),
        500 => Err(CliError::Remote(report::write_failure(&response.json()?))),
        _ => answer(response),
    }
}

fn answer(response: Response) -> Result<Checked, CliError> {
    match response.status {
        200..=299 => {
            let body = response.json()?;
            Ok(Checked::Valid(response, body))
        }
        422 => {
            let body = response.json()?;
            Ok(Checked::Invalid(response, body))
        }
        _ => response
            .into_result()
            .map(|_| unreachable!("a failure status")),
    }
}

/// The error for a refused configuration: its diagnostics, and the daemon's body on standard
/// output when the output is JSON.
pub(super) fn refusal(
    context: &Context<'_>,
    file: &str,
    response: &Response,
    body: &Value,
) -> CliError {
    match emit_json(context, response) {
        Ok(()) => CliError::Local(report::invalid(file, body)),
        Err(error) => error,
    }
}

fn emit_json(context: &Context<'_>, response: &Response) -> Result<(), CliError> {
    if matches!(context.output(), Output::Json) && !context.quiet() {
        render::emit_bytes(&[response.checked_json_body()?, b"\n"].concat())?;
    }
    Ok(())
}

/// The daemon's body when the output is JSON, the rendered document otherwise.
pub(super) fn print(
    context: &Context<'_>,
    response: &Response,
    document: &Document,
) -> Result<(), CliError> {
    if matches!(context.output(), Output::Json) {
        render::print_json_body(response, context.quiet())
    } else {
        context.print_document(document)
    }
}
