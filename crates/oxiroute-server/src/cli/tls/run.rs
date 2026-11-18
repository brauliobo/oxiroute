use serde_json::json;

use super::TlsCommand;
use crate::cli::{
    context::{Context, Execute},
    error::CliError,
};

impl Execute for TlsCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        match self {
            Self::List => context.show("/api/v1/tls"),
            Self::Reconcile { certificate } => context.mutate(
                "POST",
                "/api/v1/tls/reconcile",
                &json!({ "certificate": certificate }),
            ),
        }
    }
}
