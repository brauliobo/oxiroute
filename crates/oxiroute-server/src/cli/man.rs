use std::io::{self, Write};

use clap::{Args, Command, CommandFactory as _};
use clap_mangen::{
    Man,
    roff::{Roff, bold, roman},
};

use super::{
    Cli,
    binary::Binary,
    context::{Context, Execute},
    error::CliError,
    help::{ENVIRONMENT, EXIT_CATEGORIES, FILES},
    render,
};

type Section<'a> = &'a dyn Fn(&mut dyn Write) -> io::Result<()>;

const PRELUDE: &[u8] = b".ie \\n(.g .ds Aq \\(aq\n.el .ds Aq '\n";

#[derive(Args)]
pub(crate) struct ManArgs {
    /// Command name the page documents: `oxiroute` or its short alias `oxr`.
    #[arg(long, value_enum, default_value_t = Binary::Oxiroute)]
    name: Binary,
}

impl ManArgs {
    fn page(&self) -> Result<Vec<u8>, CliError> {
        let name = self.name.as_str();
        let mut command = Cli::command().name(name).bin_name(name);
        command.build();
        let man = Man::new(command.clone()).manual("OxiRoute Manual");
        let reference = reference(&command);
        let sections: [Section<'_>; 7] = [
            &|out| man.render_title(out),
            &|out| man.render_name_section(out),
            &|out| man.render_synopsis_section(out),
            &|out| man.render_description_section(out),
            &|out| man.render_options_section(out),
            &|out| reference.to_writer(out),
            &|out| man.render_version_section(out),
        ];
        let mut page = Vec::new();
        for render in sections {
            let mut section = Vec::new();
            render(&mut section).map_err(CliError::local)?;
            page.extend_from_slice(if page.is_empty() {
                &section
            } else {
                section.strip_prefix(PRELUDE).unwrap_or(&section)
            });
        }
        Ok(page)
    }
}

impl Execute for ManArgs {
    fn run(&self, _: &Context<'_>) -> Result<(), CliError> {
        render::emit_bytes(&self.page()?)
    }
}

fn reference(command: &Command) -> Roff {
    let mut roff = Roff::default();
    roff.control("SH", ["COMMANDS"]);
    commands(&mut roff, command);
    roff.control("SH", ["EXIT STATUS"]);
    definitions(
        &mut roff,
        EXIT_CATEGORIES
            .iter()
            .map(|(code, meaning)| (code.to_string(), *meaning)),
    );
    roff.control("SH", ["ENVIRONMENT"]);
    definitions(
        &mut roff,
        ENVIRONMENT
            .iter()
            .map(|(name, meaning)| ((*name).to_owned(), *meaning)),
    );
    roff.control("SH", ["FILES"]);
    definitions(
        &mut roff,
        FILES
            .iter()
            .map(|(path, meaning)| ((*path).to_owned(), *meaning)),
    );
    roff
}

fn commands(roff: &mut Roff, command: &Command) {
    for subcommand in command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
    {
        if subcommand.has_subcommands() {
            commands(roff, subcommand);
            continue;
        }
        let usage = subcommand.clone().render_usage().to_string();
        let usage = usage.strip_prefix("Usage: ").unwrap_or(&usage);
        let about = subcommand
            .get_about()
            .map(ToString::to_string)
            .unwrap_or_default();
        roff.control("TP", []);
        roff.text([bold(usage)]);
        roff.text([roman(about)]);
    }
}

fn definitions<'a>(roff: &mut Roff, entries: impl Iterator<Item = (String, &'a str)>) {
    for (term, meaning) in entries {
        roff.control("TP", []);
        roff.text([bold(term)]);
        roff.text([roman(meaning)]);
    }
}

#[cfg(test)]
mod tests;
