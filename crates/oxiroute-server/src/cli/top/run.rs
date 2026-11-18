use std::io::{IsTerminal as _, stdin, stdout};

use super::{TopArgs, app::App, once, rows::View, session, source};
use crate::cli::{
    context::{Context, Execute},
    error::CliError,
    global::Output,
    render,
};

impl Execute for TopArgs {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        let client = context.client()?;
        if client.endpoint().is_unix() {
            return Err(CliError::Local(
                "top reads the management API; registry sockets do not serve statistics".into(),
            ));
        }
        if self.json || matches!(context.output(), Output::Json) {
            return render::print_json_body(
                &client.get(source::STATS_PATH, true)?,
                context.quiet(),
            );
        }
        let sort = self.start_sort()?;
        let first = source::sample(&client, self.start_view() == View::Requests)?;
        if self.once || !(stdin().is_terminal() && stdout().is_terminal()) {
            if !context.quiet() {
                let views = self.snapshot_views();
                let sorts = views
                    .iter()
                    .map(|view| {
                        if *view == self.start_view() {
                            sort
                        } else {
                            view.default_sort()
                        }
                    })
                    .collect::<Vec<_>>();
                render::emit(&once::render(&first, &views, &sorts, &self.filter))?;
            }
            return Ok(());
        }
        session::run(
            App::new(self.start_view(), sort, self.filter.clone(), self.interval),
            client,
            first,
        )
    }
}
