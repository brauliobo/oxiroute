use super::RequestsCommand;
use crate::cli::{
    context::{Context, Execute},
    error::CliError,
};

impl Execute for RequestsCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        context.show(&self.path())
    }
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
