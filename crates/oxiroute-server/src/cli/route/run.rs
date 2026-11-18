use serde_json::Value;

use super::{RouteCommand, target::Request};
use crate::cli::{
    context::{Context, Execute},
    error::CliError,
    view::View,
};

impl Execute for RouteCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        let Self::Test {
            target,
            path,
            method,
            listener,
        } = self;
        let request = Request::parse(target, path.as_deref()).map_err(CliError::Local)?;
        let answer = context
            .client()?
            .get(&request.query(method, listener.as_deref()), true)?
            .json()?;
        if answer
            .get("matches")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
        {
            return Err(CliError::Status(
                404,
                format!(
                    "route_not_found: no route serves {}",
                    request.describe(method)
                ),
            ));
        }
        context.print(&answer, View::RouteTest)
    }
}
