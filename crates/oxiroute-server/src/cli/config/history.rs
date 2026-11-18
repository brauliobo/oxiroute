use serde_json::Value;

use super::{
    apply::{baseline, finish},
    args::{DiffArgs, RollbackArgs},
    daemon::{self, Checked, Stored},
    report,
};
use crate::cli::{
    context::Context,
    error::CliError,
    global::Output,
    render::{self, Block, Document, Field},
};

const MAX_REVISIONS: usize = 2;
const CURRENT: &str = "current";

pub(super) fn list(context: &Context<'_>) -> Result<(), CliError> {
    let response = daemon::history(&context.client()?)?;
    if matches!(context.output(), Output::Json) {
        return render::print_json_body(&response, context.quiet());
    }
    context.print_document(&listing(&response.json()?))
}

fn listing(body: &Value) -> Document {
    let current = body.get("current").unwrap_or(&Value::Null);
    let text = |value: &Value, key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    let number = |value: &Value, key: &str| value.get(key).and_then(Value::as_u64);
    let rows = body
        .get("entries")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|entry| {
            vec![
                Field::text("id", text(entry, "id").as_deref()),
                Field::timestamp("saved", number(entry, "at")),
                Field::bytes("size", number(entry, "bytes")),
                Field::text(
                    "state",
                    Some(
                        if entry.get("current").and_then(Value::as_bool) == Some(true) {
                            "current text"
                        } else {
                            ""
                        },
                    ),
                ),
            ]
        })
        .collect();
    Document::new(vec![
        Block::Record {
            name: "current".into(),
            fields: vec![
                Field::text("current", text(current, "id").as_deref()),
                Field::bytes("size", number(current, "bytes")),
                Field::revision("revision", text(body, "diskRevision").as_deref()),
            ],
        },
        Block::Rows {
            name: "versions".into(),
            rows,
        },
    ])
}

pub(super) fn show(context: &Context<'_>, id: &str) -> Result<(), CliError> {
    let response = daemon::history_entry(&context.client()?, id)?;
    if matches!(context.output(), Output::Json) {
        return render::print_json_body(&response, context.quiet());
    }
    let body = response.json()?;
    let Some(text) = body.get("text").and_then(Value::as_str) else {
        return Err(CliError::Remote(
            "the daemon withheld this version because it may hold secrets that cannot be redacted \
             reliably; read it from the history directory on the host"
                .into(),
        ));
    };
    if context.quiet() {
        return Ok(());
    }
    render::emit(text)?;
    let redacted = body
        .pointer("/redaction/redacted")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if redacted > 0 {
        render::diagnose(&format!(
            "oxiroute: {redacted} inline secret(s) are shown as <redacted> placeholders"
        ));
    }
    Ok(())
}

pub(super) fn diff(context: &Context<'_>, args: &DiffArgs) -> Result<(), CliError> {
    if args.rev.len() > MAX_REVISIONS {
        return Err(CliError::Local(
            "--rev takes at most two versions: FROM and TO".into(),
        ));
    }
    let from = &args.rev[0];
    let to = args.rev.get(1).map_or(CURRENT, String::as_str);
    let response = daemon::history_diff(&context.client()?, from, to)?;
    let body = response.json()?;
    let identical = body.get("identical").and_then(Value::as_bool) == Some(true);
    if matches!(context.output(), Output::Json) {
        render::print_json_body(&response, context.quiet())?;
    } else if !context.quiet() {
        print(&body, args.text, from, to)?;
    }
    if args.exit_code && !identical {
        return Err(CliError::Differs);
    }
    Ok(())
}

fn print(body: &Value, text: bool, from: &str, to: &str) -> Result<(), CliError> {
    if body.get("unresolved").and_then(Value::as_bool) == Some(true) {
        render::diagnose(&format!(
            "oxiroute: {from} or {to} no longer validates, so its settings cannot be compared; the texts differ"
        ));
        return Ok(());
    }
    match body.get("diff").filter(|diff| !report::is_equal(diff)) {
        Some(diff) => render::emit(&report::changes(diff))?,
        None => render::diagnose(&format!("no setting differs between {from} and {to}")),
    }
    if !text {
        return Ok(());
    }
    match body.get("textDiff").and_then(Value::as_str) {
        Some(unified) if !unified.is_empty() => render::emit(unified),
        Some(_) => {
            render::diagnose(&format!("the texts of {from} and {to} are identical"));
            Ok(())
        }
        None => {
            render::diagnose("no text diff: the texts may hold secrets that cannot be redacted");
            Ok(())
        }
    }
}

pub(super) fn rollback(context: &Context<'_>, args: &RollbackArgs) -> Result<(), CliError> {
    let client = context.client()?;
    let (stored, _) = Stored::read(&client, "disk")?;
    stored.ensure_writable()?;
    let baseline = baseline(&client, args.wait)?;
    let label = format!("history entry {}", args.id);
    match daemon::rollback(&client, &args.id, &stored.disk_revision)? {
        Checked::Invalid(response, body) => Err(daemon::refusal(context, &label, &response, &body)),
        Checked::Valid(response, body) => {
            daemon::print(context, &response, &report::saved(&body))?;
            finish(context, &client, &body, baseline.as_ref())
        }
    }
}
