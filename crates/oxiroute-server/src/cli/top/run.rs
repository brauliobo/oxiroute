use std::{
    collections::BTreeMap,
    io::{IsTerminal as _, stdin, stdout},
};

use super::{
    TopArgs,
    app::{App, Settings},
    export::{self, Kind, Plan},
    history::Window,
    model::Filter,
    prefs::{Prefs, Store},
    session,
    source::{self, Wants},
    theme::Capabilities,
};
use crate::cli::{
    context::{Context, Execute},
    error::CliError,
    global::Output,
    net::Client,
    render,
};

fn environment(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

impl TopArgs {
    fn kind(&self, context: &Context<'_>) -> Kind {
        match (
            self.printing.json || matches!(context.output(), Output::Json),
            self.printing.csv,
        ) {
            (true, _) => Kind::Json,
            (_, true) => Kind::Csv,
            _ => Kind::Text,
        }
    }

    fn preferences(&self, store: &Store) -> Result<(Prefs, Option<String>), CliError> {
        if self.terminal.reset_layout {
            store.reset()?;
        }
        let (mut prefs, notice) = match store.load() {
            Ok(prefs) => (prefs, None),
            Err(error) => (Prefs::default(), Some(error.to_string())),
        };
        if let Some(theme) = self.theme {
            prefs.theme = theme;
        }
        Ok((prefs, notice))
    }

    fn plan(&self, prefs: Prefs, window: Window) -> Result<Plan, CliError> {
        let view = self.start_view();
        Ok(Plan {
            views: self.snapshot_views(),
            sorts: BTreeMap::from([(view, self.start_sort()?)]),
            filter: Filter::parse(&self.filter),
            window,
            units: prefs.units,
            prefs,
            top: usize::from(self.top),
        })
    }

    fn print(&self, client: &Client, context: &Context<'_>, prefs: Prefs) -> Result<(), CliError> {
        let window = self.window.unwrap_or(Window::Short);
        let plan = self.plan(prefs, window)?;
        let history = export::measure(client, &plan)?;
        if context.quiet() {
            return Ok(());
        }
        render::emit(&export::render(&history, &plan, self.kind(context))?)
    }

    fn interact(
        &self,
        client: Client,
        prefs: Prefs,
        notice: Option<String>,
        store: &Store,
    ) -> Result<(), CliError> {
        let view = self.start_view();
        let window = self.window.unwrap_or(prefs.window);
        let sort = self.start_sort()?;
        let key = view.columns()[sort.column].key;
        let wants = Wants::for_view(
            view,
            key,
            &Filter::parse(&self.filter),
            usize::from(self.top),
        );
        let first = source::sample(&client, &wants)?;
        let app = App::new(Settings {
            view,
            sort: Some(sort),
            filter: self.filter.clone(),
            limit: usize::from(self.top),
            mouse: !self.terminal.no_mouse,
            prefs,
            window,
            capabilities: Capabilities::detect(self.color, self.terminal.ascii, &environment),
            notice,
        });
        session::run(app, client, first, self.interval, store)
    }
}

impl Execute for TopArgs {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        let client = context.client()?;
        if client.endpoint().is_unix() {
            return Err(CliError::Local(
                "top reads the management API; registry sockets do not serve statistics".into(),
            ));
        }
        let store = Store::standard(&environment)?;
        let (prefs, notice) = self.preferences(&store)?;
        let machine = self.machine_readable() || matches!(context.output(), Output::Json);
        let scripted =
            machine || self.printing.once || !(stdin().is_terminal() && stdout().is_terminal());
        if scripted {
            if let Some(notice) = notice {
                render::diagnose(&notice);
            }
            self.print(&client, context, prefs)
        } else {
            self.interact(client, prefs, notice, &store)
        }
    }
}
