use std::{fs, io::IsTerminal as _, path::PathBuf};

use clap::{Args, Subcommand};
use oxiroute_config_source::{ConfigFormat, render_config, resolve_source_with_format};
use serde_json::{Value, json};

use super::{
    apply::{baseline, finish},
    args::ComposeFormat,
    daemon::{self, Checked},
    edit::{Scratch, ask, confirmed, review, run_editor},
    input::{InputFormat, SourceInput},
};
use crate::cli::{context::Context, error::CliError, render, view::View};

const IF_CONFIG_REVISION: &str = "If-Config-Revision";

#[derive(Subcommand)]
pub(crate) enum ConfigFilesCommand {
    /// List the daemon's root configuration file and every directory-jailed included file.
    List,
    /// Print the authored text of one directory-jailed configuration file on the daemon.
    Show {
        /// Path relative to the daemon's configuration directory (as listed by `config files list`).
        path: String,
    },
    /// Edit a directory-jailed configuration file in $VISUAL or $EDITOR, validate it against the
    /// full configuration graph, and save it back to the daemon.
    Edit {
        /// Path relative to the daemon's configuration directory (or a new relative path with `--create`).
        path: String,
        /// Create a new included file if it does not yet exist.
        #[arg(long)]
        create: bool,
        /// Apply without asking for confirmation.
        #[arg(long)]
        yes: bool,
        /// Wait until the new revision is active or rejected, up to --timeout (30s by default).
        #[arg(long)]
        wait: bool,
    },
}

#[derive(Args)]
pub(crate) struct ConvertArgs {
    /// Source configuration file in any supported format.
    pub(super) file: PathBuf,
    /// Explicit input format when the file extension does not identify it.
    #[arg(long = "from", value_enum, value_name = "FORMAT")]
    pub(super) from: Option<InputFormat>,
    /// Target configuration format to render.
    #[arg(long = "to", value_enum, value_name = "FORMAT")]
    pub(super) to: ComposeFormat,
    /// Write converted output to this file instead of stdout.
    #[arg(id = "dest_file", short = 'o', long = "out", value_name = "FILE")]
    pub(super) output: Option<PathBuf>,
}

pub(super) fn run_files(
    context: &Context<'_>,
    command: &ConfigFilesCommand,
) -> Result<(), CliError> {
    match command {
        ConfigFilesCommand::List => context.show("/api/v1/config/files", View::Generic),
        ConfigFilesCommand::Show { path } => show_file(context, path),
        ConfigFilesCommand::Edit {
            path,
            create,
            yes,
            wait,
        } => edit_file(context, path, *create, *yes, *wait),
    }
}

pub(super) fn run_convert(context: &Context<'_>, args: &ConvertArgs) -> Result<(), CliError> {
    let input = SourceInput::read(&args.file, args.from)?;
    let resolved = resolve_source_with_format(
        &input.path,
        input.text.as_bytes(),
        input.format.config_format(),
    )
    .map_err(|error| CliError::Local(format!("cannot convert {}: {error}", args.file.display())))?;
    let target: ConfigFormat = args.to.into();
    let rendered = render_config(target, &resolved.config).map_err(CliError::local)?;
    if let Some(dest) = &args.output {
        fs::write(dest, &rendered).map_err(|error| {
            CliError::Local(format!("cannot write {}: {error}", dest.display()))
        })?;
        if !context.quiet() {
            render::diagnose(&format!(
                "converted {} to {} ({})",
                args.file.display(),
                dest.display(),
                target.wire_name()
            ));
        }
        Ok(())
    } else {
        render::emit(&rendered)
    }
}

fn show_file(context: &Context<'_>, rel_path: &str) -> Result<(), CliError> {
    let client = context.client()?;
    let encoded = encode_rel_path(rel_path);
    let body = client
        .get(&format!("/api/v1/config/files/{encoded}"), true)?
        .json()?;
    if matches!(context.output(), crate::cli::global::Output::Json) {
        return context.print(&body, View::Generic);
    }
    if body
        .pointer("/redaction/withheld")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(CliError::Local(
            "the daemon withheld this file because one of its inline secrets cannot be safely redacted"
                .into(),
        ));
    }
    let text = body
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::Remote("daemon returned no file text".into()))?;
    render::emit(text)
}

fn edit_file(
    context: &Context<'_>,
    rel_path: &str,
    create: bool,
    yes: bool,
    wait: bool,
) -> Result<(), CliError> {
    let client = context.client()?;
    let encoded = encode_rel_path(rel_path);
    let (original, file_revision, ext) = load_editable_file(&client, &encoded, rel_path, create)?;
    let scratch = Scratch::create(&original, &ext)?;
    let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    let label = scratch.path.display().to_string();

    loop {
        run_editor(&scratch.path)?;
        let text = fs::read_to_string(&scratch.path)
            .map_err(|error| CliError::Local(format!("cannot read {label}: {error}")))?;
        if !create && text == original {
            render::diagnose("oxiroute: no changes");
            return Ok(());
        }
        let payload = json!({
            "path": rel_path,
            "text": text,
            "textDiff": true,
        });
        let response =
            client.json_unchecked("POST", "/api/v1/config/files/validate", &payload, &[])?;
        let body = response.json()?;
        let checked = if response.is_success() {
            Checked::Valid(response, body)
        } else {
            Checked::Invalid(response, body)
        };
        match checked {
            Checked::Invalid(response, body) => {
                let refusal = daemon::refusal(context, rel_path, &response, &body);
                if interactive {
                    render::diagnose(&format!("oxiroute: {refusal}"));
                    if ask("open the editor again? [Y/n] ", true)? {
                        continue;
                    }
                }
                return Err(scratch.keep(refusal));
            }
            Checked::Valid(_, body) => {
                review(context, &body)?;
                if !yes && !confirmed(interactive)? {
                    render::diagnose(&format!(
                        "oxiroute: nothing applied, your edit is kept in {label}"
                    ));
                    scratch.persist();
                    return Ok(());
                }
                let base = baseline(&client, wait)?;
                let save_body = json!({ "text": text, "create": create });
                let headers: Vec<(&str, &str)> = file_revision
                    .as_deref()
                    .map(|rev| vec![(IF_CONFIG_REVISION, rev)])
                    .unwrap_or_default();
                let saved_json =
                    save_file_request(context, &client, &encoded, rel_path, &save_body, &headers)
                        .map_err(|error| scratch.keep(error))?;
                return finish(context, &client, &saved_json, base.as_ref());
            }
        }
    }
}

fn load_editable_file(
    client: &crate::cli::net::Client,
    encoded: &str,
    rel_path: &str,
    create: bool,
) -> Result<(String, Option<String>, String), CliError> {
    if create {
        let ext = rel_path
            .rsplit_once('.')
            .map_or("oxi", |(_, ext)| ext)
            .to_owned();
        return Ok((String::new(), None, ext));
    }
    let body = client
        .get(&format!("/api/v1/config/files/{encoded}"), true)?
        .json()?;
    if !body
        .get("configWritable")
        .and_then(Value::as_bool)
        .unwrap_or(true)
    {
        return Err(CliError::Remote(
            "configuration directory is not writable by the daemon".into(),
        ));
    }
    if body
        .pointer("/redaction/withheld")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(CliError::Local(
            "the daemon withheld this file because one of its inline secrets cannot be safely redacted"
                .into(),
        ));
    }
    let text = body
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::Remote("daemon returned no file text".into()))?
        .to_owned();
    let revision = body
        .get("fileRevision")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let format = body
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("txt")
        .to_owned();
    Ok((text, revision, format))
}

fn save_file_request(
    context: &Context<'_>,
    client: &crate::cli::net::Client,
    encoded: &str,
    rel_path: &str,
    save_body: &Value,
    headers: &[(&str, &str)],
) -> Result<Value, CliError> {
    let saved_resp = client.json_unchecked(
        "PUT",
        &format!("/api/v1/config/files/{encoded}"),
        save_body,
        headers,
    )?;
    let saved_json = saved_resp.json()?;
    if !saved_resp.is_success() {
        return Err(daemon::refusal(context, rel_path, &saved_resp, &saved_json));
    }
    Ok(saved_json)
}

fn encode_rel_path(rel_path: &str) -> String {
    rel_path
        .split('/')
        .map(|segment| {
            let mut out = String::with_capacity(segment.len());
            for byte in segment.bytes() {
                if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                    out.push(byte as char);
                } else {
                    use std::fmt::Write as _;
                    let _ = write!(out, "%{byte:02X}");
                }
            }
            out
        })
        .collect::<Vec<_>>()
        .join("/")
}
