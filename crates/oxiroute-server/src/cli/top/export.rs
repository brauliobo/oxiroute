mod csv;
mod json;
mod text;

use std::{collections::BTreeMap, thread, time::Duration};

use super::{
    history::{History, Sampling, Window},
    layout::Tier,
    model::{Arranged, Arrangement, Filter, Groups, Sort, Units},
    prefs::Prefs,
    source::{self, Wants},
    views::View,
};
use crate::cli::{error::CliError, net::Client};

pub(crate) struct Plan {
    pub(crate) views: Vec<View>,
    pub(crate) sorts: BTreeMap<View, Sort>,
    pub(crate) filter: Filter,
    pub(crate) window: Window,
    pub(crate) units: Units,
    pub(crate) prefs: Prefs,
    pub(crate) top: usize,
}

pub(crate) struct Printed {
    pub(crate) view: View,
    pub(crate) sort: Sort,
    pub(crate) arranged: Arranged,
    pub(crate) columns: Vec<usize>,
}

impl Plan {
    fn sort(&self, view: View) -> Sort {
        self.sorts
            .get(&view)
            .copied()
            .unwrap_or_else(|| view.default_sort())
    }

    fn wants(&self) -> Wants {
        self.views.iter().fold(Wants::default(), |wants, view| {
            let key = view.columns()[self.sort(*view).column].key;
            wants.merged(Wants::for_view(*view, key, &self.filter, self.top))
        })
    }
}

pub(crate) fn measure(client: &Client, plan: &Plan) -> Result<History, CliError> {
    let wants = plan.wants();
    let mut history = History::default();
    history.push(source::sample(client, &wants)?);
    if let Some(span) = plan.window.millis() {
        thread::sleep(Duration::from_millis(span));
        history.push(source::sample(client, &wants)?);
    }
    Ok(history)
}

pub(crate) fn headline_sampling<'a>(
    history: &'a History,
    plan: &Plan,
) -> Result<Sampling<'a>, CliError> {
    history
        .sampling(plan.window, View::Listeners)
        .ok_or_else(|| CliError::Remote("the server returned no statistics".into()))
}

pub(crate) fn printed(history: &History, plan: &Plan) -> Vec<Printed> {
    plan.views
        .iter()
        .filter_map(|view| {
            let sampling = history.sampling(plan.window, *view)?;
            let sort = plan.sort(*view);
            let arrangement = Arrangement {
                sort,
                filter: &plan.filter,
                groups: &Groups::new(true),
                lock: None,
                units: plan.units,
            };
            Some(Printed {
                view: *view,
                sort,
                arranged: Arranged::new(view.table(&sampling), &arrangement),
                columns: plan.prefs.visible(*view, Tier::Wide),
            })
        })
        .collect()
}

pub(crate) fn render(history: &History, plan: &Plan, kind: Kind) -> Result<String, CliError> {
    let sampling = headline_sampling(history, plan)?;
    let tables = printed(history, plan);
    let text = match kind {
        Kind::Text => text::render(&sampling, plan, &tables),
        Kind::Json => json::render(&sampling, plan, &tables)?,
        Kind::Csv => csv::render(plan, &tables),
    };
    Ok(text)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
    Text,
    Json,
    Csv,
}
