use std::{
    fs::{self, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
};

use oxiroute_config_source::{ConfigFormat, render_config};

use super::{InitArgs, args::Kind, templates};
use crate::cli::{
    config::ComposeFormat,
    context::{Context, Execute},
    error::CliError,
    render,
};

impl Execute for InitArgs {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        let destination = self
            .file
            .clone()
            .unwrap_or_else(|| PathBuf::from(format!("oxiroute.{}", extension(self.format))));
        let base = base_of(&destination)?;
        let text = self.render(&base)?;
        if destination == Path::new("-") {
            return render::emit(&text);
        }
        self.write(&destination, &text)?;
        if matches!(self.kind, Kind::Static) {
            fs::create_dir_all(base.join(templates::PUBLIC_DIRECTORY)).map_err(CliError::local)?;
        }
        if context.quiet() {
            return Ok(());
        }
        render::emit(&format!(
            "wrote {file} ({kind} configuration)\nnext:\n  oxiroute token create --token-file {token}\n  \
             oxiroute doctor {file}\n  oxiroute serve {file}\n",
            file = destination.display(),
            kind = name(self.kind),
            token = base.join(templates::TOKEN_FILE).display(),
        ))
    }
}

fn base_of(destination: &Path) -> Result<PathBuf, CliError> {
    let directory = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::path::absolute(directory).map_err(CliError::local)
}

impl InitArgs {
    pub(super) fn render(&self, base: &Path) -> Result<String, CliError> {
        let config = templates::config(self.kind, base).map_err(CliError::local)?;
        render_config(ConfigFormat::from(self.format), &config).map_err(CliError::local)
    }

    pub(super) fn write(&self, destination: &Path, text: &str) -> Result<(), CliError> {
        let mut options = OpenOptions::new();
        options.write(true);
        if self.force {
            options.create(true).truncate(true);
        } else {
            options.create_new(true);
        }
        let failed = |error: std::io::Error| {
            let hint = if destination.exists() && !self.force {
                "; use --force to replace it"
            } else {
                ""
            };
            CliError::local(format!(
                "cannot write {}: {error}{hint}",
                destination.display()
            ))
        };
        options
            .open(destination)
            .and_then(|mut file| file.write_all(text.as_bytes()))
            .map_err(failed)
    }
}

pub(super) const fn extension(format: ComposeFormat) -> &'static str {
    match format {
        ComposeFormat::Kdl => "kdl",
        ComposeFormat::Lua => "lua",
        ComposeFormat::Uci => "uci",
        ComposeFormat::Hocon => "conf",
    }
}

const fn name(kind: Kind) -> &'static str {
    match kind {
        Kind::Proxy => "proxy",
        Kind::Static => "static",
        Kind::Rtmp => "rtmp",
    }
}
