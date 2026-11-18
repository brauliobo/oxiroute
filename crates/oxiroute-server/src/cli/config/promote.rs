use serde_json::Value;

use super::{
    apply::{baseline, finish},
    daemon::{self, Checked, Stored},
    report,
};
use crate::cli::{
    context::Context,
    error::CliError,
    render::{Document, Field},
};

pub(in crate::cli) fn promote_overlay(context: &Context<'_>, wait: bool) -> Result<(), CliError> {
    let client = context.client()?;
    let (stored, _) = Stored::read(&client, "disk")?;
    stored.ensure_writable()?;
    let baseline = baseline(&client, wait)?;
    match daemon::promote(&client, &stored.disk_revision)? {
        Checked::Invalid(response, body) => {
            Err(daemon::refusal(context, "the overlay", &response, &body))
        }
        Checked::Valid(response, body) if body.get("promoted") == Some(&Value::from(0)) => {
            daemon::print(context, &response, &nothing_to_promote())
        }
        Checked::Valid(response, body) => {
            daemon::print(context, &response, &report::saved(&body))?;
            finish(context, &client, &body, baseline.as_ref())
        }
    }
}

fn nothing_to_promote() -> Document {
    Document::record(
        "overlay",
        vec![
            Field::text("outcome", Some("nothing_to_promote")),
            Field::number("promoted", Some(0)),
        ],
    )
}
