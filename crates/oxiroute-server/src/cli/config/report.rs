use std::{fmt::Write as _, path::Path};

use serde_json::Value;

use crate::cli::render::{Block, Document, Field, Record};

const MAX_VALUE_CHARS: usize = 100;

fn hint(code: &str) -> Option<&'static str> {
    Some(match code {
        "E_SYNTAX" => {
            "fix the syntax at the reported line; `oxiroute config check FILE` checks a file offline"
        }
        "E_INVALID_VALUE" => {
            "the path names the rejected setting and the message the rule it broke"
        }
        "E_INVALID_FIELD" => {
            "a field is missing, unknown or of the wrong type; compare with `oxiroute config show --active`"
        }
        "E_NATIVE_SOURCE" => {
            "a native source the file references could not be imported; `oxiroute import ... --emit report` says why"
        }
        "E_FORMAT_MISMATCH" => {
            "the daemon runs a file in another format; pass --convert to store the equivalent, or convert it with `oxiroute config compose --format FORMAT FILE`"
        }
        "E_REVISION_CONFLICT" => {
            "the file changed on disk since it was read; run the command again"
        }
        "E_RUNTIME_PREPARE" => {
            "the file is valid but cannot become a running generation; check listener binds, certificate files and the management token file"
        }
        "E_CONFIG_READ_ONLY" => {
            "add the configuration directory to ReadWritePaths in a systemd drop-in (see the packaging README), or edit the file on the host"
        }
        "E_CONFIG_NOT_WRITABLE" => {
            "give the service user write access to the configuration directory, or edit the file on the host"
        }
        _ => return None,
    })
}

pub(super) fn unwritable(block: &str) -> String {
    let why = match block {
        "read_only_filesystem" => "its directory is on a read-only mount",
        "permission_denied" => "the service user may not create files in its directory",
        _ => "its directory cannot be written",
    };
    let fix = if block == "read_only_filesystem" {
        "Under the packaged systemd unit (ProtectSystem=strict) add the directory to ReadWritePaths with a drop-in, \
         for example `ReadWritePaths=/etc/oxiroute`, then run `systemctl daemon-reload` and restart the service"
    } else {
        "Give the service user write access to the directory"
    };
    format!(
        "the daemon cannot replace its configuration file: {why}. {fix}; or edit the file on the host and run \
         `oxiroute generation reload --wait`"
    )
}

pub(super) fn write_failure(body: &Value) -> String {
    let lines = diagnostic_lines(None, body);
    if lines.is_empty() {
        "the daemon could not write the configuration file".into()
    } else {
        format!(
            "the daemon could not write the configuration file\n{}",
            lines.join("\n")
        )
    }
}

pub(super) fn invalid(file: &str, body: &Value) -> String {
    let lines = diagnostic_lines(Some(Path::new(file)), body);
    if lines.is_empty() {
        let code = body.pointer("/error/code").and_then(Value::as_str);
        let message = body.pointer("/error/message").and_then(Value::as_str);
        return format!(
            "{file} is invalid: {}",
            message.or(code).unwrap_or("the daemon refused it")
        );
    }
    format!("{file} is invalid\n{}", lines.join("\n"))
}

fn diagnostic_lines(file: Option<&Path>, body: &Value) -> Vec<String> {
    let diagnostics = body
        .get("diagnostics")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|diagnostic| diagnostic.get("severity").and_then(Value::as_str) != Some("warning"));
    diagnostics
        .map(|diagnostic| render(file, diagnostic))
        .collect()
}

fn render(file: Option<&Path>, diagnostic: &Value) -> String {
    let field = |name| diagnostic.get(name).and_then(Value::as_str);
    let code = field("code").unwrap_or("error");
    let mut line = format!("  {code}");
    if let (Some(file), Some(number)) = (file, diagnostic.get("line").and_then(Value::as_u64)) {
        let _ = write!(line, " {}:{number}", file.display());
    }
    if let Some(path) = field("path") {
        let _ = write!(line, " at {path}");
    }
    if let Some(reason) = field("detail").or_else(|| field("message")) {
        let _ = write!(line, ": {reason}");
    }
    if let Some(hint) = hint(code) {
        let _ = write!(line, "\n    hint: {hint}");
    }
    line
}

fn text<'a>(value: &'a Value, name: &str) -> Option<&'a str> {
    value.get(name).and_then(Value::as_str)
}

fn flag(value: &Value, name: &str) -> Option<bool> {
    value.get(name).and_then(Value::as_bool)
}

pub(super) fn validation(body: &Value) -> Document {
    let mut blocks = vec![Block::Record {
        name: "validation".into(),
        fields: vec![
            Field::text("result", Some("valid")),
            Field::text("format", text(body, "format")),
            Field::revision("candidate", text(body, "candidateRevision")),
            Field::revision("active", text(body, "activeRevision")),
            Field::flag("restart_required", flag(body, "restartRequired")),
            Field::flag("compositional", flag(body, "compositional")),
        ],
    }];
    blocks.extend(notices(body));
    Document::new(blocks)
}

pub(super) fn saved(body: &Value) -> Document {
    let mut blocks = vec![Block::Record {
        name: "apply".into(),
        fields: vec![
            Field::text("outcome", text(body, "outcome")),
            Field::flag("written", flag(body, "written")),
            Field::revision("disk", text(body, "diskRevision")),
            Field::revision("candidate", text(body, "candidateRevision")),
            Field::revision("active", text(body, "activeRevision")),
            Field::flag("restart_required", flag(body, "restartRequired")),
        ],
    }];
    blocks.extend(notices(body));
    Document::new(blocks)
}

fn notices(body: &Value) -> Option<Block> {
    let rows = body
        .get("diagnostics")
        .and_then(Value::as_array)?
        .iter()
        .map(|diagnostic| -> Record {
            vec![
                Field::text("code", text(diagnostic, "code")),
                Field::text("severity", text(diagnostic, "severity")),
                Field::text("message", text(diagnostic, "message")),
            ]
        })
        .collect::<Vec<_>>();
    (!rows.is_empty()).then_some(Block::Rows {
        name: "diagnostics".into(),
        rows,
    })
}

/// One line per changed setting: `+` added, `-` removed, `~` changed, `^` reordered.
pub(super) fn changes(comparison: &Value) -> String {
    let mut out = String::new();
    for change in comparison
        .get("changes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let path = text(change, "path").unwrap_or("");
        let shown = |name| change.get(name).map(compact);
        let (marker, detail) = match text(change, "kind") {
            Some("added") => ('+', shown("new").unwrap_or_default()),
            Some("removed") => ('-', shown("old").unwrap_or_default()),
            Some("reordered") => (
                '^',
                format!(
                    "{} -> {}",
                    shown("old").unwrap_or_default(),
                    shown("new").unwrap_or_default()
                ),
            ),
            _ => (
                '~',
                format!(
                    "{} -> {}",
                    shown("old").unwrap_or_else(|| "null".into()),
                    shown("new").unwrap_or_else(|| "null".into())
                ),
            ),
        };
        let _ = writeln!(out, "{marker} {path}: {detail}");
    }
    if flag(comparison, "truncated") == Some(true) {
        out.push_str("... more changes are not listed\n");
    }
    out
}

fn compact(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() <= MAX_VALUE_CHARS {
        return text;
    }
    let cut = text.chars().take(MAX_VALUE_CHARS).collect::<String>();
    format!("{cut}...")
}

pub(super) fn is_equal(comparison: &Value) -> bool {
    flag(comparison, "equal") == Some(true)
}
