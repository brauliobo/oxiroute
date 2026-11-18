mod plain;
mod table;

use std::io::{self, Write as _};

use serde_json::Value;

use super::{error::CliError, global::Output, net::Response};

pub(super) fn emit(text: &str) {
    print!("{text}");
}

pub(super) fn print_value(value: &Value, output: Output, quiet: bool) -> Result<(), CliError> {
    if quiet {
        return Ok(());
    }
    match output {
        Output::Json => println!(
            "{}",
            serde_json::to_string(value).map_err(|error| CliError::Local(error.to_string()))?
        ),
        Output::Table => table::print(value),
        Output::Plain => plain::print(value),
    }
    Ok(())
}

/// `--output json` of a response that needs no selection: the body the server wrote, after a syntax check, without
/// building a `Value` tree and printing it again.
pub(super) fn print_json_body(response: &Response, quiet: bool) -> Result<(), CliError> {
    let body = response.checked_json_body()?;
    if quiet {
        return Ok(());
    }
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(body)
        .and_then(|()| stdout.write_all(b"\n"))
        .map_err(|error| CliError::Local(error.to_string()))
}
