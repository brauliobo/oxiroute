use serde_json::json;

use super::{
    TokenCommand,
    file::{self, TOKEN_CHARACTERS},
};
use crate::cli::{
    context::{Context, Execute},
    error::CliError,
    view::View,
};

impl Execute for TokenCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        let path = context.token_destination()?;
        let outcome = match self {
            Self::Create => file::create(&path).map(|()| "created"),
            Self::Rotate => file::rotate(&path).map(|()| "rotated"),
        }?;
        context.print(
            &json!({
                "outcome": outcome,
                "file": path.display().to_string(),
                "characters": TOKEN_CHARACTERS,
            }),
            View::Generic,
        )
    }
}
