use std::{
    cell::OnceCell,
    io,
    path::{Path, PathBuf},
};

use oxiroute_config::Management;

use crate::{
    cli::{error::CliError, fs::read_regular_file},
    config::coordinator::{CanonicalConfigCoordinator, ConfigLoadOutcome},
};

pub(in crate::cli) const CONTEXT_ENV: &str = "OXIROUTE_CONTEXT";
pub(in crate::cli) const ENDPOINT_ENV: &str = "OXIROUTE_ENDPOINT";
pub(in crate::cli) const MANAGEMENT_TOKEN_ENV: &str = "OXIROUTE_MANAGEMENT_TOKEN_FILE";
pub(in crate::cli) const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:9900";
const PACKAGE_ENV_FILE: &str = "/etc/oxiroute/oxiroute.env";
const PACKAGE_CONFIG_PREFIX: &str = "/etc/oxiroute/oxiroute.";
const PACKAGE_TOKEN_FILE: &str = "/etc/oxiroute/management.token";
const MAX_PACKAGE_ENV_BYTES: usize = 8 * 1024;

fn package_config_paths() -> Vec<PathBuf> {
    oxiroute_config_source::ALL_FORMATS
        .iter()
        .flat_map(|spec| spec.extensions.iter())
        .map(|ext| PathBuf::from(format!("{PACKAGE_CONFIG_PREFIX}{ext}")))
        .collect()
}

/// Where a packaged install keeps the settings a local daemon's clients need.
pub(in crate::cli) struct Package {
    env_file: PathBuf,
    configs: Vec<PathBuf>,
    token_file: PathBuf,
    management: OnceCell<Option<Management>>,
}

impl Package {
    pub(in crate::cli) fn installed() -> Self {
        Self::at(
            PACKAGE_ENV_FILE.into(),
            package_config_paths(),
            PACKAGE_TOKEN_FILE.into(),
        )
    }

    pub(in crate::cli) const fn at(
        env_file: PathBuf,
        configs: Vec<PathBuf>,
        token_file: PathBuf,
    ) -> Self {
        Self {
            env_file,
            configs,
            token_file,
            management: OnceCell::new(),
        }
    }

    #[cfg(test)]
    pub(in crate::cli) fn config_paths(&self) -> &[PathBuf] {
        &self.configs
    }

    pub(in crate::cli) fn discovered_endpoint(&self) -> Option<String> {
        self.env_value(ENDPOINT_ENV).ok().flatten().or_else(|| {
            self.management()
                .map(|management| format!("http://{}", management.bind))
        })
    }

    pub(in crate::cli) fn token_path(
        &self,
        explicit: Option<&Path>,
    ) -> Result<Option<PathBuf>, CliError> {
        if let Some(path) = explicit {
            return Ok(Some(path.to_owned()));
        }
        if let Some(path) = self.env_value(MANAGEMENT_TOKEN_ENV)? {
            return Ok(Some(PathBuf::from(path)));
        }
        if let Some(path) = self
            .management()
            .and_then(|management| management.token_file.clone())
        {
            return Ok(Some(path));
        }
        match std::fs::symlink_metadata(&self.token_file) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Ok(_) | Err(_) => Ok(Some(self.token_file.clone())),
        }
    }

    pub(in crate::cli) fn token_destination(
        &self,
        explicit: Option<&Path>,
    ) -> Result<PathBuf, CliError> {
        Ok(self
            .token_path(explicit)?
            .unwrap_or_else(|| self.token_file.clone()))
    }

    fn management(&self) -> Option<&Management> {
        self.management
            .get_or_init(|| self.configs.iter().find_map(|path| management_of(path)))
            .as_ref()
    }

    fn env_value(&self, key: &str) -> Result<Option<String>, CliError> {
        let bytes = match read_regular_file(&self.env_file, MAX_PACKAGE_ENV_BYTES, false) {
            Ok(bytes) => bytes,
            Err(error) if error.is_missing() => return Ok(None),
            Err(error) => return Err(env_error(&self.env_file, &error)),
        };
        let contents = std::str::from_utf8(&bytes)
            .map_err(|_| env_error(&self.env_file, &"the file is not UTF-8"))?;
        let mut found = None;
        for line in contents.lines().map(str::trim) {
            let Some((name, value)) = line.split_once('=').filter(|(name, _)| *name == key) else {
                continue;
            };
            let value = value.trim();
            if found.is_some() || value.is_empty() || value.chars().any(char::is_control) {
                return Err(env_error(
                    &self.env_file,
                    &format!("{name} is repeated, empty or has control characters"),
                ));
            }
            found = Some(value.to_owned());
        }
        Ok(found)
    }
}

fn management_of(path: &Path) -> Option<Management> {
    let coordinator = CanonicalConfigCoordinator::new(path).ok()?;
    match coordinator.load() {
        ConfigLoadOutcome::Loaded(document) => document.validated_config.to_draft().management,
        ConfigLoadOutcome::Rejected(_) => None,
    }
}

fn env_error(path: &Path, cause: &dyn std::fmt::Display) -> CliError {
    CliError::Local(format!(
        "cannot read the packaged environment file {}: {cause}",
        path.display()
    ))
}
