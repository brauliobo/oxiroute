use super::{
    ConfigCommand, apply::apply, check, compare, compose::compose, edit::edit, fmt::fmt, show::show,
};
use crate::cli::{
    context::{Context, Execute},
    error::CliError,
    render,
    view::View,
};

impl Execute for ConfigCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        match self {
            Self::Check { config } => render::emit(&check::report(config)?),
            Self::Compose { format, configs } => render::emit(&compose(configs, (*format).into())?),
            Self::Get => context.show("/api/v1/config", View::Config),
            Self::Show(args) => show(context, args),
            Self::Validate(args) => compare::validate(context, args),
            Self::Diff(args) => compare::diff(context, args),
            Self::Apply(args) => apply(context, args),
            Self::Edit(args) => edit(context, args),
            Self::Fmt(args) => fmt(context, args),
        }
    }
}
