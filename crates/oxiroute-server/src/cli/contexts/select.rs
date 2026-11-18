use std::{path::Path, time::Duration};

use super::{
    source::TokenSource,
    store::{Entry, Store},
};
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
        let entry = file
            .contexts
            .remove(&name)
            .ok_or_else(|| store.unknown(&name))?;
        Ok(Some(Self { name, entry }))
    }

    pub(crate) fn token_file(&self) -> Option<&Path> {
        match self.entry.token.as_ref()? {
            TokenSource::File(path) => Some(path),
            TokenSource::Env(_) | TokenSource::Command(_) => None,
        }
    }

    pub(crate) fn timeout(&self) -> Option<Duration> {
        self.entry
            .timeout
            .as_deref()
            .and_then(|timeout| parse_timeout(timeout).ok())
    }
}
