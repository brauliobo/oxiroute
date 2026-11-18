use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};

use super::source::TokenSource;
use crate::cli::{
    error::CliError,
    fs::{read_regular_file, write_private},
    net::Endpoint,
    parse::parse_timeout,
};

const SCHEMA_VERSION: u32 = 1;
const MAX_BYTES: usize = 256 * 1024;
const MAX_CONTEXTS: usize = 256;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Entry {
    pub(crate) endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) token: Option<TokenSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) timeout: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ContextFile {
    schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) current: Option<String>,
    #[serde(default)]
    pub(super) contexts: BTreeMap<String, Entry>,
}

impl Default for ContextFile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            current: None,
            contexts: BTreeMap::new(),
        }
    }
}

pub(super) struct Store {
    pub(super) path: PathBuf,
}

impl Store {
    pub(super) const fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub(super) fn standard() -> Result<Self, CliError> {
        let variable = |name| std::env::var_os(name).filter(|value| !value.is_empty());
        let base = match variable("XDG_CONFIG_HOME") {
            Some(base) => PathBuf::from(base),
            None => PathBuf::from(variable("HOME").ok_or_else(|| {
                CliError::Local(
                    "cannot place the contexts file: neither XDG_CONFIG_HOME nor HOME is set"
                        .into(),
                )
            })?)
            .join(".config"),
        };
        Ok(Self::at(base.join("oxiroute").join("contexts")))
    }

    pub(super) fn load(&self) -> Result<ContextFile, CliError> {
        let bytes = match read_regular_file(&self.path, MAX_BYTES, true) {
            Ok(bytes) => bytes,
            Err(error) if error.is_missing() => return Ok(ContextFile::default()),
            Err(error) => {
                return Err(CliError::Local(format!(
                    "contexts file {}: {error} (it must be a regular file with mode 0600: chmod 600 it)",
                    self.path.display()
                )));
            }
        };
        let file: ContextFile = serde_json::from_slice(&bytes).map_err(|error| {
            CliError::Local(format!(
                "contexts file {} is not valid at line {}, column {}: it holds {{\"schemaVersion\": \
                 {SCHEMA_VERSION}, \"current\": NAME, \"contexts\": {{NAME: {{\"endpoint\": URL, \
                 \"token\": {{\"file\": PATH}} or {{\"env\": VARIABLE}} or {{\"command\": [PROGRAM, \
                 ARG...]}}, \"timeout\": \"10s\"}}}}}}; a token itself is never stored in it",
                self.path.display(),
                error.line(),
                error.column()
            ))
        })?;
        self.check(&file)?;
        Ok(file)
    }

    fn check(&self, file: &ContextFile) -> Result<(), CliError> {
        let invalid = |name: &str, reason: &str| {
            CliError::Local(format!(
                "contexts file {}: context {name}: {reason}",
                self.path.display()
            ))
        };
        if file.schema_version != SCHEMA_VERSION {
            return Err(CliError::Local(format!(
                "contexts file {} has schemaVersion {}, this oxiroute reads {SCHEMA_VERSION}",
                self.path.display(),
                file.schema_version
            )));
        }
        if file.contexts.len() > MAX_CONTEXTS {
            return Err(CliError::Local(format!(
                "contexts file {} holds more than {MAX_CONTEXTS} contexts",
                self.path.display()
            )));
        }
        if let Some(current) = file
            .current
            .as_deref()
            .filter(|name| !file.contexts.contains_key(*name))
        {
            return Err(CliError::Local(format!(
                "contexts file {}: the current context {current} is not defined",
                self.path.display()
            )));
        }
        for (name, entry) in &file.contexts {
            if !is_name(name) {
                return Err(invalid(
                    name,
                    "the name must be 1 to 64 letters, digits, `.`, `_` or `-`",
                ));
            }
            Endpoint::parse(&entry.endpoint).map_err(|error| invalid(name, &error.to_string()))?;
            if entry
                .token
                .as_ref()
                .is_some_and(|token| !token.is_well_formed())
            {
                return Err(invalid(
                    name,
                    "the token source needs an absolute file path, a variable name or a command",
                ));
            }
            if let Some(timeout) = &entry.timeout {
                parse_timeout(timeout)
                    .map_err(|reason| invalid(name, &format!("timeout: {reason}")))?;
            }
        }
        Ok(())
    }

    pub(super) fn save(&self, file: &ContextFile) -> Result<(), CliError> {
        self.check(file)?;
        let mut text = serde_json::to_string_pretty(file).map_err(CliError::local)?;
        text.push('\n');
        write_private(&self.path, text.as_bytes()).map_err(|error| {
            CliError::Local(format!("cannot write {}: {error}", self.path.display()))
        })
    }
}

pub(super) fn is_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
}
