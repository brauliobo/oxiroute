use std::{
    fs, io,
    path::{Path, PathBuf},
};

use super::{Prefs, VERSION};
use crate::cli::error::CliError;

const MAX_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct Store {
    path: PathBuf,
}

impl Store {
    pub(crate) const fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub(crate) fn standard(variable: &dyn Fn(&str) -> Option<String>) -> Result<Self, CliError> {
        let set = |name: &str| variable(name).filter(|value| !value.is_empty());
        let base = match set("XDG_STATE_HOME") {
            Some(base) => PathBuf::from(base),
            None => PathBuf::from(set("HOME").ok_or_else(|| {
                CliError::Local(
                    "cannot place the top preferences: neither XDG_STATE_HOME nor HOME is set"
                        .into(),
                )
            })?)
            .join(".local")
            .join("state"),
        };
        Ok(Self::at(base.join("oxiroute").join("top.json")))
    }

    pub(crate) fn load(&self) -> Result<Prefs, CliError> {
        let metadata = match fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Prefs::default()),
            Err(error) => return Err(self.failure("read", &error)),
        };
        if metadata.len() > MAX_BYTES {
            return Err(self.invalid("it is larger than 64 KiB"));
        }
        let bytes = fs::read(&self.path).map_err(|error| self.failure("read", &error))?;
        let prefs: Prefs =
            serde_json::from_slice(&bytes).map_err(|error| self.invalid(&error.to_string()))?;
        if prefs.version == VERSION {
            Ok(prefs)
        } else {
            Err(self.invalid(&format!(
                "it has version {}, this build reads {VERSION}",
                prefs.version
            )))
        }
    }

    pub(crate) fn save(&self, prefs: &Prefs) -> Result<(), CliError> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .map_err(|error| self.failure("create the directory of", &error))?;
        let text = serde_json::to_string_pretty(prefs).map_err(CliError::local)?;
        let scratch = self.path.with_extension("json.tmp");
        fs::write(&scratch, format!("{text}\n")).map_err(|error| self.failure("write", &error))?;
        fs::rename(&scratch, &self.path).map_err(|error| self.failure("replace", &error))
    }

    pub(crate) fn reset(&self) -> Result<(), CliError> {
        match fs::remove_file(&self.path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                Err(self.failure("remove", &error))
            }
            _ => Ok(()),
        }
    }

    fn failure(&self, action: &str, error: &io::Error) -> CliError {
        CliError::Local(format!(
            "cannot {action} the top preferences {}: {error}",
            self.path.display()
        ))
    }

    fn invalid(&self, reason: &str) -> CliError {
        CliError::Local(format!(
            "the top preferences {} are not valid: {reason}; `oxiroute top --reset-layout` removes them",
            self.path.display()
        ))
    }
}
