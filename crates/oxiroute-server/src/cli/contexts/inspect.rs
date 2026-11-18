use std::path::PathBuf;

use super::{source::TokenSource, store::Store};
use crate::cli::error::CliError;

pub(in crate::cli) struct TokenFile {
    pub(in crate::cli) context: String,
    pub(in crate::cli) path: PathBuf,
    pub(in crate::cli) selected: bool,
}

pub(in crate::cli) struct Inspection {
    pub(in crate::cli) path: PathBuf,
    pub(in crate::cli) contexts: usize,
    pub(in crate::cli) selected: Option<String>,
    pub(in crate::cli) token_files: Vec<TokenFile>,
}

pub(in crate::cli) fn inspect(requested: Option<&str>) -> Result<Option<Inspection>, CliError> {
    let store = match Store::standard() {
        Ok(store) => store,
        Err(error) if requested.is_some() => return Err(error),
        Err(_) => return Ok(None),
    };
    inspect_in(&store, requested)
}

pub(super) fn inspect_in(
    store: &Store,
    requested: Option<&str>,
) -> Result<Option<Inspection>, CliError> {
    let file = store.load()?;
    let selected = requested
        .map(str::to_owned)
        .or_else(|| file.current.clone());
    if let Some(name) = selected
        .as_deref()
        .filter(|name| !file.contexts.contains_key(*name))
    {
        return Err(store.unknown(name));
    }
    if file.contexts.is_empty() {
        return Ok(None);
    }
    let token_files = file
        .contexts
        .iter()
        .filter_map(|(name, entry)| match &entry.token {
            Some(TokenSource::File(path)) => Some(TokenFile {
                context: name.clone(),
                path: path.clone(),
                selected: selected.as_deref() == Some(name),
            }),
            _ => None,
        })
        .collect();
    Ok(Some(Inspection {
        path: store.path.clone(),
        contexts: file.contexts.len(),
        selected,
        token_files,
    }))
}
