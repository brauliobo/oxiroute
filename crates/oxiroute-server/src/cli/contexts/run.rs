use serde_json::{Value, json};

use super::{
    ContextCommand,
    args::AddArgs,
    select::Selected,
    source::TokenSource,
    store::{ContextFile, Entry, Store, is_name},
};
use crate::cli::{
    context::{Context, Execute},
    error::CliError,
    global::Output,
    net::span,
    render,
    view::View,
};

impl Execute for ContextCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        let store = Store::standard()?;
        match self {
            Self::List => list(context, &store),
            Self::Add(args) => add(context, &store, args),
            Self::Use { name } => select(context, &store, name),
            Self::Remove { name } => remove(context, &store, name),
            Self::Current => current(context, &store),
        }
    }
}

fn list(context: &Context<'_>, store: &Store) -> Result<(), CliError> {
    let file = store.load()?;
    let contexts = file
        .contexts
        .iter()
        .map(|(name, entry)| {
            json!({
                "name": name,
                "current": file.current.as_deref() == Some(name),
                "endpoint": entry.endpoint,
                "tokenSource": entry.token.as_ref().map(TokenSource::kind),
                "tokenRef": entry.token.as_ref().map(TokenSource::reference),
                "timeout": entry.timeout,
            })
        })
        .collect::<Vec<_>>();
    context.print(
        &json!({ "schemaVersion": 1, "file": store.path, "current": file.current, "contexts": contexts }),
        View::Contexts,
    )
}

fn add(context: &Context<'_>, store: &Store, args: &AddArgs) -> Result<(), CliError> {
    if !is_name(&args.name) {
        return Err(CliError::Local(
            "a context name is 1 to 64 letters, digits, `.`, `_` or `-`".into(),
        ));
    }
    let mut file = store.load()?;
    if file.contexts.contains_key(&args.name) && !args.replace {
        return Err(CliError::Local(format!(
            "context {} exists: pass --replace to overwrite it",
            args.name
        )));
    }
    file.contexts.insert(args.name.clone(), entry(args));
    if args.make_current {
        file.current = Some(args.name.clone());
    }
    store.save(&file)?;
    outcome(context, "added", &args.name, &file)
}

fn entry(args: &AddArgs) -> Entry {
    let token = args
        .token_from_file
        .clone()
        .map(TokenSource::File)
        .or_else(|| args.token_from_env.clone().map(TokenSource::Env))
        .or_else(|| args.token_from_command.clone().map(TokenSource::Command));
    Entry {
        endpoint: args.address.clone(),
        token,
        timeout: args.default_timeout.map(span),
    }
}

fn select(context: &Context<'_>, store: &Store, name: &str) -> Result<(), CliError> {
    let mut file = store.load()?;
    if !file.contexts.contains_key(name) {
        return Err(store.unknown(name));
    }
    file.current = Some(name.to_owned());
    store.save(&file)?;
    outcome(context, "selected", name, &file)
}

fn remove(context: &Context<'_>, store: &Store, name: &str) -> Result<(), CliError> {
    let mut file = store.load()?;
    if file.contexts.remove(name).is_none() {
        return Err(store.unknown(name));
    }
    if file.current.as_deref() == Some(name) {
        file.current = None;
    }
    store.save(&file)?;
    outcome(context, "removed", name, &file)
}

fn current(context: &Context<'_>, store: &Store) -> Result<(), CliError> {
    let selected = Selected::resolve_in(store, context.requested_context())?;
    let name = selected.map(|selected| selected.name).ok_or_else(|| {
        CliError::Local(
            "no context is selected: `oxiroute context use NAME`, --context or OXIROUTE_CONTEXT choose one"
                .into(),
        )
    })?;
    match context.output() {
        Output::Json => context.print(&json!({ "context": name }), View::Generic),
        Output::Table | Output::Plain if context.quiet() => Ok(()),
        Output::Table | Output::Plain => render::emit(&format!("{name}\n")),
    }
}

fn outcome(
    context: &Context<'_>,
    outcome: &str,
    name: &str,
    file: &ContextFile,
) -> Result<(), CliError> {
    let value: Value = json!({ "outcome": outcome, "context": name, "current": file.current });
    context.print(&value, View::Generic)
}
