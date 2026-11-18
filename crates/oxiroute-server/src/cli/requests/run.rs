use serde_json::Value;

use super::RequestsCommand;
use crate::cli::{
    context::{Context, Execute},
    error::CliError,
    view::View,
};

impl Execute for RequestsCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        match self {
            Self::List { .. } => context.show(&self.path(), View::Requests),
            Self::Show { id } => show(context, id, &self.path()),
        }
    }
}

fn show(context: &Context<'_>, id: &str, path: &str) -> Result<(), CliError> {
    let value = context.client()?.get(path, true)?.json()?;
    if value
        .get("requests")
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty)
    {
        return Err(CliError::Status(
            404,
            format!("request_not_found: no in-flight request has id {id}"),
        ));
    }
    context.print(&value, View::Request)
}

impl RequestsCommand {
    pub(super) fn path(&self) -> String {
        match self {
            Self::Show { id } => format!("/api/v1/requests?requestId={id}"),
            Self::List {
                pool,
                route,
                older_than,
                phase,
                limit,
            } => {
                let filters = [
                    pool.as_ref().map(|pool| format!("pool={pool}")),
                    route.as_ref().map(|route| format!("route={route}")),
                    phase.as_ref().map(|phase| format!("phase={phase}")),
                    older_than.map(|age| format!("min_age_ms={age}")),
                    Some(format!("limit={limit}")),
                ];
                format!(
                    "/api/v1/requests?{}",
                    filters.into_iter().flatten().collect::<Vec<_>>().join("&")
                )
            }
        }
    }
}
