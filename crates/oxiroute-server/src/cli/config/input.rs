use std::path::{Path, PathBuf};

use clap::ValueEnum;
use oxiroute_config_source::{ConfigFormat, render_config, resolve_source_with_format};
use serde_json::{Value, json};

use crate::cli::{
    error::CliError,
    fs::{MAX_FILE_BYTES, read_regular_file, unreadable_config},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum InputFormat {
    Kdl,
    Lua,
    Hocon,
    Uci,
    Json,
}

impl InputFormat {
    pub(super) const fn wire(self) -> &'static str {
        match self {
            Self::Kdl => "kdl",
            Self::Lua => "lua",
            Self::Hocon => "hocon",
            Self::Uci => "uci",
            Self::Json => "json",
        }
    }

    pub(super) const fn config_format(self) -> ConfigFormat {
        match self {
            Self::Kdl => ConfigFormat::Kdl,
            Self::Lua => ConfigFormat::Lua,
            Self::Hocon | Self::Json => ConfigFormat::Hocon,
            Self::Uci => ConfigFormat::Uci,
        }
    }

    fn of_extension(path: &Path) -> Result<Self, CliError> {
        let Some(extension) = path.extension() else {
            return Ok(Self::Kdl);
        };
        match extension.to_string_lossy().to_ascii_lowercase().as_str() {
            "kdl" | "kdl2" => Ok(Self::Kdl),
            "lua" => Ok(Self::Lua),
            "uci" => Ok(Self::Uci),
            "hocon" | "conf" => Ok(Self::Hocon),
            "json" => Ok(Self::Json),
            other => Err(CliError::Local(format!(
                "cannot tell the format of {} from the extension `.{other}`: pass --format kdl, \
                 lua, hocon, uci or json",
                path.display()
            ))),
        }
    }

    pub(super) fn named(wire: &str) -> Option<Self> {
        Self::value_variants()
            .iter()
            .copied()
            .find(|format| format.wire() == wire)
    }
}

/// A configuration file read for a command that sends it to the daemon.
pub(super) struct SourceInput {
    pub(super) path: PathBuf,
    pub(super) format: InputFormat,
    pub(super) text: String,
}

impl SourceInput {
    pub(super) fn read(path: &Path, format: Option<InputFormat>) -> Result<Self, CliError> {
        let format = match format {
            Some(format) => format,
            None => InputFormat::of_extension(path)?,
        };
        let bytes = read_regular_file(path, MAX_FILE_BYTES, false)
            .map_err(|error| unreadable_config(path, &error))?;
        let text = String::from_utf8(bytes)
            .map_err(|_| CliError::Local(format!("{} is not UTF-8 text", path.display())))?;
        let text = if format == InputFormat::Json {
            typed_document(path, &text)?
        } else {
            text
        };
        Ok(Self {
            path: path.to_owned(),
            format,
            text,
        })
    }

    pub(super) fn request(&self, text_diff: bool) -> Value {
        json!({ "text": self.text, "format": self.format.wire(), "textDiff": text_diff })
    }

    pub(super) fn matches(&self, stored: InputFormat) -> bool {
        self.format.config_format() == stored.config_format()
    }

    /// The text in the format of the daemon's file: the equivalent configuration, rendered, so the
    /// comments and the logic of the input are not part of it.
    pub(super) fn converted(&self, stored: InputFormat) -> Result<Self, CliError> {
        let resolved = resolve_source_with_format(
            &self.path,
            self.text.as_bytes(),
            self.format.config_format(),
        )
        .map_err(|error| {
            CliError::Local(format!("cannot convert {}: {error}", self.path.display()))
        })?;
        let text =
            render_config(stored.config_format(), &resolved.config).map_err(CliError::local)?;
        Ok(Self {
            path: self.path.clone(),
            format: stored,
            text,
        })
    }
}

fn typed_document(path: &Path, text: &str) -> Result<String, CliError> {
    let value: Value = serde_json::from_str(text).map_err(|error| {
        CliError::Local(format!("{} is not valid JSON: {error}", path.display()))
    })?;
    let config = match value.get("config") {
        Some(config) if config.is_object() => config,
        _ => &value,
    };
    serde_json::to_string_pretty(config).map_err(CliError::local)
}
