mod document;

use std::io::{self, Write as _};

use serde_json::Value;

pub(crate) use self::document::{Block, Document, Field, Record};
use super::{error::CliError, global::Output, net::Response, view::View};

pub(super) fn emit(text: &str) -> Result<(), CliError> {
    emit_bytes(text.as_bytes())
}

pub(super) fn emit_bytes(bytes: &[u8]) -> Result<(), CliError> {
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(bytes)
        .and_then(|()| stdout.flush())
        .map_err(|error| match error.kind() {
            io::ErrorKind::BrokenPipe => CliError::OutputClosed,
            _ => CliError::Local(format!("cannot write to standard output: {error}")),
        })
}

pub(super) fn diagnose(line: &str) {
    let _ = writeln!(io::stderr().lock(), "{line}");
}

pub(super) fn print_value(
    value: &Value,
    view: View,
    output: Output,
    quiet: bool,
) -> Result<(), CliError> {
    if quiet {
        return Ok(());
    }
    match output {
        Output::Json => {
            let json = serde_json::to_string(value).map_err(CliError::local)?;
            emit(&format!("{json}\n"))
        }
        Output::Table | Output::Plain => print_document(&view.document(value), output),
    }
}

pub(super) fn print_document(document: &Document, output: Output) -> Result<(), CliError> {
    match output {
        Output::Plain => emit(&document.plain()),
        Output::Table | Output::Json => {
            let text = document.table();
            emit(&text.stdout)?;
            text.notices.iter().for_each(|notice| diagnose(notice));
            Ok(())
        }
    }
}

/// `--output json` of a response that needs no selection: the body the server wrote, after a syntax check, without
/// building a `Value` tree and printing it again.
pub(super) fn print_json_body(response: &Response, quiet: bool) -> Result<(), CliError> {
    let body = response.checked_json_body()?;
    if quiet {
        return Ok(());
    }
    emit_bytes(&[body, b"\n"].concat())
}
