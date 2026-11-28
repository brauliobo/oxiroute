use super::{
    ConfigCommand, apply::apply, check, compare, compose::compose, edit::edit, files, fmt::fmt,
    history, resource, show::show,
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
            Self::Check {
                config,
                input_format,
                no_probe,
                dump,
                format,
            } => {
                let dump_format = dump.then(|| format.unwrap_or_default().into());
                let explicit_in = input_format.map(super::input::InputFormat::config_format);
                render::emit(&check::report_with_format(
                    config,
                    explicit_in,
                    !*no_probe,
                    dump_format,
                )?)
            }
            Self::Convert(args) => files::run_convert(context, args),
            Self::Files { command } => files::run_files(context, command),
            Self::Resource { command } => resource::run_resource(context, command),
            Self::Compose { format, configs } => render::emit(&compose(configs, (*format).into())?),
            Self::Get => context.show("/api/v1/config", View::Config),
            Self::Show(args) => show(context, args),
            Self::Validate(args) => compare::validate(context, args),
            Self::Diff(args) => compare::diff(context, args),
            Self::Apply(args) => apply(context, args),
            Self::Edit(args) => edit(context, args),
            Self::History => history::list(context),
            Self::Rollback(args) => history::rollback(context, args),
            Self::Fmt(args) => fmt(context, args),
        }
    }
}
