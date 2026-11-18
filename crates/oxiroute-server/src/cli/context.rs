use std::{error::Error, path::Path};

use serde_json::Value;

use super::{
    change::Plan,
    error::CliError,
    global::{Global, Output},
    net::{Client, Package, Response},
    render::{self, Document},
    view::View,
};

pub type ServeFn = fn(&Path) -> Result<(), Box<dyn Error>>;

pub(crate) trait Execute {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError>;
}

pub(crate) struct Context<'a> {
    global: &'a Global,
    package: Package,
    serve: ServeFn,
}

impl<'a> Context<'a> {
    pub(super) fn new(global: &'a Global, serve: ServeFn) -> Self {
        Self {
            global,
            package: Package::installed(),
            serve,
        }
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
        let endpoint = self.global.endpoint(&self.package)?;
        let token = if endpoint.is_unix() {
            None
        } else {
            self.global.token(&self.package)?
        };
        Ok(Client::new(endpoint, token))
    }

    /// A client for the public probes, which never read the token file.
    pub(crate) fn public_client(&self) -> Result<Client, CliError> {
        Ok(Client::new(self.global.endpoint(&self.package)?, None))
    }

    pub(crate) fn print(&self, value: &Value, view: View) -> Result<(), CliError> {
        render::print_value(value, view, self.global.output, self.global.quiet)
    }

    pub(crate) fn print_document(&self, document: &Document) -> Result<(), CliError> {
        if self.global.quiet {
            return Ok(());
        }
        render::print_document(document, self.global.output)
    }

    pub(crate) fn print_stream(&self, value: &Value, view: View) -> Result<(), CliError> {
        let output = match self.global.output {
            Output::Table => Output::Plain,
            output => output,
        };
        render::print_value(value, view, output, self.global.quiet)
    }

    pub(crate) fn print_response(&self, response: &Response, view: View) -> Result<(), CliError> {
        if matches!(self.global.output, Output::Json) && response.is_json() {
            render::print_json_body(response, self.global.quiet)
        } else {
            self.print(&response.output_value()?, view)
        }
    }

    pub(crate) fn show(&self, path: &str, view: View) -> Result<(), CliError> {
        self.print_response(&self.client()?.get(path, true)?, view)
    }

    pub(crate) fn show_named(
        &self,
        path: &str,
        field: &str,
        name: &str,
        view: View,
    ) -> Result<(), CliError> {
        let mut value = self.client()?.get(path, true)?.output_value()?;
        let entries = value
            .get_mut(field)
            .and_then(Value::as_array_mut)
            .ok_or_else(|| CliError::Local(format!("response has no {field} array")))?;
        entries.retain(|entry| entry.get("name").and_then(Value::as_str) == Some(name));
        if entries.is_empty() {
            return Err(CliError::Status(404, format!("{field}_not_found")));
        }
        self.print(&value, view)
    }

    pub(crate) fn show_text(&self, path: &str) -> Result<(), CliError> {
        let response = self.public_client()?.get(path, false)?;
        match self.global.output {
            Output::Json => self.print_response(&response, View::Generic),
            Output::Table | Output::Plain if self.global.quiet => Ok(()),
            Output::Table | Output::Plain => {
                let text = std::str::from_utf8(&response.body)
                    .map_err(|_| CliError::Remote("endpoint returned invalid UTF-8".into()))?;
                render::emit(text)
            }
        }
    }

    pub(crate) fn mutate(&self, method: &str, path: &str, body: &Value) -> Result<(), CliError> {
        self.mutate_as(method, path, body, View::Generic)
    }

    pub(crate) fn mutate_as(
        &self,
        method: &str,
        path: &str,
        body: &Value,
        view: View,
    ) -> Result<(), CliError> {
        self.print_response(&self.client()?.mutation_json(method, path, body)?, view)
    }

    pub(crate) fn change(
        &self,
        method: &str,
        path: &str,
        body: &Value,
        plan: &Plan,
    ) -> Result<(), CliError> {
        let client = self.client()?;
        if matches!(self.global.output, Output::Json) {
            return self.print_response(&client.mutation_json(method, path, body)?, View::Generic);
        }
        let observed = plan.observe(&client)?;
        client.mutation_json(method, path, body)?;
        self.print_document(&plan.summary(&observed))
    }
}
