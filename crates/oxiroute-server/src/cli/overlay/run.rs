use serde_json::json;

use super::OverlayCommand;
use crate::cli::{
    config,
    context::{Context, Execute},
    error::CliError,
    view::View,
};

const OVERLAY: &str = "/api/v1/overlay";

impl Execute for OverlayCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        match self {
            Self::Show => context.show(OVERLAY, View::Overlay),
            Self::Clear => context.print_response(
                &context
                    .client()?
                    .json("POST", &format!("{OVERLAY}/clear"), &json!({}), &[])?,
                View::Generic,
            ),
            Self::Promote { wait } => config::promote_overlay(context, *wait),
        }
    }
}
