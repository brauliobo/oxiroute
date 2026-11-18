use serde_json::Value;

use super::{args::ShowArgs, daemon::Stored, history};
use crate::cli::{
    context::Context,
    error::CliError,
    fs::{MAX_FILE_BYTES, read_regular_file, unreadable_config},
    global::Output,
    render,
};

pub(super) fn show(context: &Context<'_>, args: &ShowArgs) -> Result<(), CliError> {
    if let Some(file) = &args.file {
        let bytes = read_regular_file(file, MAX_FILE_BYTES, false)
            .map_err(|error| unreadable_config(file, &error))?;
        return if context.quiet() {
            Ok(())
        } else {
            render::emit_bytes(&bytes)
        };
    }
    if let Some(id) = &args.rev {
        return history::show(context, id);
    }
    let client = context.client()?;
    let view = if args.active { "active" } else { "disk" };
    let (stored, response) = Stored::read(&client, view)?;
    if matches!(context.output(), Output::Json) {
        return render::print_json_body(&response, context.quiet());
    }
    let text = stored.readable()?;
    if context.quiet() {
        return Ok(());
    }
    render::emit(text)?;
    notices(&stored.body, args.active);
    Ok(())
}

fn notices(body: &Value, active: bool) {
    if active {
        render::diagnose(
            "oxiroute: this is the running configuration rendered canonically, not the authored text",
        );
    }
    let redacted = body
        .pointer("/redaction/redacted")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if redacted > 0 {
        render::diagnose(&format!(
            "oxiroute: {redacted} inline secret(s) are shown as <redacted> placeholders and are \
             restored when the text is applied"
        ));
    }
}
