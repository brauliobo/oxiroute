use std::time::Duration;

use super::store::{Entry, Store};
use crate::cli::{error::CliError, parse::parse_timeout};

pub(crate) struct Selected {
    pub(crate) name: String,
    pub(crate) entry: Entry,
}

impl Selected {
    pub(crate) fn resolve(requested: Option<&str>) -> Result<Option<Self>, CliError> {
        let store = match Store::standard() {
            Ok(store) => store,
            Err(error) if requested.is_some() => return Err(error),
            Err(_) => return Ok(None),
        };
        Self::resolve_in(&store, requested)
    }

    pub(super) fn resolve_in(
        store: &Store,
        requested: Option<&str>,
    ) -> Result<Option<Self>, CliError> {
        let mut file = store.load()?;
        let Some(name) = requested.map(str::to_owned).or_else(|| file.current.take()) else {
            return Ok(None);
        };
        let entry = file.contexts.remove(&name).ok_or_else(|| {
            CliError::Local(format!(
                "no context named {name} in {}: `oxiroute context list` shows the defined ones",
                store.path.display()
            ))
        })?;
        Ok(Some(Self { name, entry }))
    }

    pub(crate) fn timeout(&self) -> Option<Duration> {
        self.entry
            .timeout
            .as_deref()
            .and_then(|timeout| parse_timeout(timeout).ok())
    }
}
