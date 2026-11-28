use std::{
    fs::{self, OpenOptions},
    io::{self, IsTerminal as _, Write as _},
    os::unix::fs::OpenOptionsExt as _,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

use super::{
    apply::{baseline, finish, store},
    args::EditArgs,
    daemon::{self, Checked, Stored},
    input::SourceInput,
    report,
};
use crate::cli::{context::Context, error::CliError, render};

const DEFAULT_EDITOR: &str = "vi";

pub(super) fn edit(context: &Context<'_>, args: &EditArgs) -> Result<(), CliError> {
    let client = context.client()?;
    let (stored, _) = Stored::read(&client, "disk")?;
    stored.ensure_writable()?;
    let original = stored.readable()?.to_owned();
    let scratch = Scratch::create(&original, stored.format.wire())?;
    let interactive = io::stdin().is_terminal() && io::stderr().is_terminal();
    let label = scratch.path.display().to_string();
    loop {
        run_editor(&scratch.path)?;
        let text = fs::read_to_string(&scratch.path)
            .map_err(|error| CliError::Local(format!("cannot read {label}: {error}")))?;
        if text.trim().is_empty() {
            render::diagnose("oxiroute: the edited text is empty, nothing was changed");
            return Ok(());
        }
        if text == original {
            render::diagnose("oxiroute: no changes");
            return Ok(());
        }
        let input = SourceInput {
            path: scratch.path.clone(),
            format: stored.format,
            text,
        };
        match daemon::validate(&client, &input.request(true))? {
            Checked::Invalid(response, body) => {
                let refusal = daemon::refusal(context, &label, &response, &body);
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
                if !args.yes && !confirmed(interactive)? {
                    render::diagnose(&format!(
                        "oxiroute: nothing applied, your edit is kept in {label}"
                    ));
                    scratch.persist();
                    return Ok(());
                }
                let baseline = baseline(&client, args.wait)?;
                let saved = store(context, &client, &stored, &input, &label)
                    .map_err(|error| scratch.keep(error))?;
                return finish(context, &client, &saved, baseline.as_ref());
            }
        }
    }
}

pub(super) fn review(context: &Context<'_>, body: &Value) -> Result<(), CliError> {
    if context.quiet() {
        return Ok(());
    }
    if let Some(comparison) = body
        .pointer("/diff/active")
        .filter(|value| !value.is_null())
    {
        render::emit(&report::changes(comparison))?;
    }
    if let Some(text) = body.get("textDiff").and_then(Value::as_str) {
        render::emit(text)?;
    }
    Ok(())
}

pub(super) fn confirmed(interactive: bool) -> Result<bool, CliError> {
    if !interactive {
        return Err(CliError::Local(
            "confirmation required: run on a terminal, or pass --yes to apply without asking"
                .into(),
        ));
    }
    ask("apply these changes? [y/N] ", false)
}

pub(super) fn ask(question: &str, default: bool) -> Result<bool, CliError> {
    let mut stderr = io::stderr().lock();
    write!(stderr, "{question}")
        .and_then(|()| stderr.flush())
        .map_err(CliError::Io)?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).map_err(CliError::Io)?;
    Ok(match answer.trim().to_ascii_lowercase().as_str() {
        "" => default,
        answer => answer.starts_with('y'),
    })
}

pub(super) fn run_editor(path: &Path) -> Result<(), CliError> {
    let editor = ["VISUAL", "EDITOR"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_EDITOR.to_owned());
    let status = Command::new("sh")
        .args(["-c", &format!("exec {editor} \"$1\""), "oxiroute-edit"])
        .arg(path)
        .status()
        .map_err(|error| CliError::Local(format!("cannot run the editor `{editor}`: {error}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(CliError::Local(format!(
            "the editor `{editor}` exited with {status}"
        )))
    }
}

pub(super) struct Scratch {
    pub(super) path: PathBuf,
    keep: bool,
}

impl Scratch {
    pub(super) fn create(text: &str, extension: &str) -> Result<Self, CliError> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "oxiroute-config-{}-{nanos}.{extension}",
            std::process::id()
        ));
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .and_then(|mut file| file.write_all(text.as_bytes()))
            .map_err(|error| {
                CliError::Local(format!("cannot create {}: {error}", path.display()))
            })?;
        Ok(Self { path, keep: false })
    }

    pub(super) fn persist(mut self) {
        self.keep = true;
    }

    pub(super) fn keep(mut self, error: CliError) -> CliError {
        self.keep = true;
        match error {
            CliError::Local(message) => CliError::Local(format!(
                "{message}\nyour edit is kept in {}",
                self.path.display()
            )),
            CliError::Status(status, message) => CliError::Status(
                status,
                format!("{message}; your edit is kept in {}", self.path.display()),
            ),
            other => other,
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_file(&self.path);
        }
    }
}
