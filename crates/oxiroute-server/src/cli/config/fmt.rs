use oxiroute_config_source::{render_config, resolve_source_with_format};

use super::{
    args::FmtArgs,
    input::{InputFormat, SourceInput},
};
use crate::{
    cli::{context::Context, error::CliError, render},
    config::diff::unified,
};

pub(super) fn fmt(context: &Context<'_>, args: &FmtArgs) -> Result<(), CliError> {
    let input = SourceInput::read(&args.file, args.format)?;
    if input.format == InputFormat::Json {
        return Err(CliError::Local(
            "fmt rewrites kdl, oxi, yaml, toml, lua, hocon and uci files; a JSON document has no canonical text form".into(),
        ));
    }
    let name = args.file.display().to_string();
    let formatted = canonical(&input, &name)?;
    let differs = formatted != input.text;
    if args.write {
        return write(context, args, &formatted, differs, &name);
    }
    if args.check {
        return verdict(context, differs, &name);
    }
    if args.diff {
        if !context.quiet() {
            render::emit(&unified(
                &input.text,
                &formatted,
                &name,
                &format!("{name} (formatted)"),
            ))?;
        }
        return verdict(context, differs, &name);
    }
    if context.quiet() {
        return Ok(());
    }
    render::emit(&formatted)
}

fn canonical(input: &SourceInput, name: &str) -> Result<String, CliError> {
    let format = input.format.config_format();
    let resolved = resolve_source_with_format(&input.path, input.text.as_bytes(), format)
        .map_err(|error| CliError::Local(format!("{name}: {error}")))?;
    if resolved.compositional {
        return Err(CliError::Local(format!(
            "{name} uses templates or native references, which fmt would expand into the final \
             configuration; format a file without them"
        )));
    }
    render_config(format, &resolved.config).map_err(CliError::local)
}

fn verdict(context: &Context<'_>, differs: bool, name: &str) -> Result<(), CliError> {
    if !differs {
        return Ok(());
    }
    if !context.quiet() {
        render::diagnose(&format!("{name} is not in canonical form"));
    }
    Err(CliError::Differs)
}

fn write(
    context: &Context<'_>,
    args: &FmtArgs,
    formatted: &str,
    differs: bool,
    name: &str,
) -> Result<(), CliError> {
    if !differs {
        return Ok(());
    }
    std::fs::write(&args.file, formatted)
        .map_err(|error| CliError::Local(format!("cannot write {name}: {error}")))?;
    if !context.quiet() {
        render::diagnose(&format!("formatted {name}"));
    }
    Ok(())
}
