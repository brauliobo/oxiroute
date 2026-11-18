use std::{error::Error, path::Path};

use serde_json::Value;

use super::{
    error::CliError,
    global::{Global, Output},
    net::{Client, Response},
    render,
};

pub type ServeFn = fn(&Path) -> Result<(), Box<dyn Error>>;

pub(crate) trait Execute {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError>;
}

pub(crate) struct Context<'a> {
    global: &'a Global,
    serve: ServeFn,
}

impl<'a> Context<'a> {
    pub(super) const fn new(global: &'a Global, serve: ServeFn) -> Self {
        Self { global, serve }
    }

    pub(crate) fn serve(&self, config: &Path) -> Result<(), CliError> {
        (self.serve)(config).map_err(CliError::startup)
    }

    pub(crate) const fn output(&self) -> Output {
        self.global.output
    }

    pub(crate) const fn quiet(&self) -> bool {
        self.global.quiet
    }

    /// A client that sends the management token wherever the endpoint needs one.
    pub(crate) fn client(&self) -> Result<Client, CliError> {
        let endpoint = self.global.endpoint()?;
        let token = if endpoint.is_unix() {
            None
        } else {
            self.global.token()?
        };
        Ok(Client::new(endpoint, token))
    }

    /// A client for the public probes, which never read the token file.
    pub(crate) fn public_client(&self) -> Result<Client, CliError> {
        Ok(Client::new(self.global.endpoint()?, None))
    }

    pub(crate) fn print(&self, value: &Value) -> Result<(), CliError> {
        render::print_value(value, self.global.output, self.global.quiet)
    }

    pub(crate) fn print_response(&self, response: &Response) -> Result<(), CliError> {
        if matches!(self.global.output, Output::Json) && response.is_json() {
            render::print_json_body(response, self.global.quiet)
        } else {
            self.print(&response.output_value()?)
        }
    }

    pub(crate) fn show(&self, path: &str) -> Result<(), CliError> {
        self.print_response(&self.client()?.get(path, true)?)
    }

    pub(crate) fn show_named(&self, path: &str, field: &str, name: &str) -> Result<(), CliError> {
        let mut value = self.client()?.get(path, true)?.output_value()?;
        let entries = value
            .get_mut(field)
            .and_then(Value::as_array_mut)
            .ok_or_else(|| CliError::Local(format!("response has no {field} array")))?;
        entries.retain(|entry| entry.get("name").and_then(Value::as_str) == Some(name));
        if entries.is_empty() {
            return Err(CliError::Status(404, format!("{field}_not_found")));
        }
        self.print(&value)
    }

    pub(crate) fn show_public(&self, path: &str) -> Result<(), CliError> {
        self.print_response(&self.public_client()?.get(path, false)?)
    }

    pub(crate) fn mutate(&self, method: &str, path: &str, body: &Value) -> Result<(), CliError> {
        self.print_response(&self.client()?.mutation_json(method, path, body)?)
    }
}
