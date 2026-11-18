use std::{fmt::Write as _, thread, time::Duration};

use clap::ValueEnum as _;
use serde_json::{Value, json};

use super::{AuditCommand, args::Filter};
use crate::cli::{
    context::{Context, Execute},
    cursor::{self, PAGE_LIMIT},
    error::CliError,
    global::Output,
    interrupt,
    net::Client,
    view::View,
};

impl Execute for AuditCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        match self {
            Self::List {
                after,
                limit,
                since: None,
                filter,
            } => context.show(&filter.path(*after, *limit), View::Audit),
            Self::List {
                since: Some(since),
                filter,
                ..
            } => {
                let recent = filter.recent(&context.client()?, *since)?;
                context.print(&json!({ "records": recent.items }), View::Audit)
            }
            Self::Status => context.show("/api/v1/audit/status", View::AuditStatus),
            Self::Tail {
                after,
                since,
                interval,
                count,
                filter,
            } => {
                interrupt::exit_when_signalled()?;
                Tail {
                    context,
                    client: context.client()?,
                    filter,
                    interval: *interval,
                    remaining: *count,
                }
                .run(*after, *since)
            }
        }
    }
}

impl Filter {
    pub(super) fn path(&self, after: u64, limit: u16) -> String {
        let mut path = format!("/api/v1/audit?after={after}&limit={limit}");
        if let Some(category) = self.category.and_then(|value| value.to_possible_value()) {
            let _ = write!(path, "&category={}", category.get_name());
        }
        if let Some(result) = self.result.and_then(|value| value.to_possible_value()) {
            let _ = write!(path, "&result={}", result.get_name());
        }
        path
    }

    fn recent(&self, client: &Client, since: Duration) -> Result<cursor::Recent, CliError> {
        cursor::recent(client, "records", since, |after| {
            self.path(after, PAGE_LIMIT)
        })
    }
}

struct Tail<'a> {
    context: &'a Context<'a>,
    client: Client,
    filter: &'a Filter,
    interval: Duration,
    remaining: Option<u64>,
}

impl Tail<'_> {
    fn run(mut self, after: Option<u64>, since: Option<Duration>) -> Result<(), CliError> {
        let mut after = match (after, since) {
            (Some(after), _) => after,
            (None, Some(since)) => {
                let recent = self.filter.recent(&self.client, since)?;
                if self.print(&recent.items)? {
                    return Ok(());
                }
                recent.cursor
            }
            (None, None) => self.latest()?,
        };
        loop {
            let page = self
                .client
                .get(&self.filter.path(after, PAGE_LIMIT), true)?
                .json()?;
            if self.print(cursor::items(&page, "records"))? {
                return Ok(());
            }
            let next = cursor::after(&page, after);
            if !(cursor::has_more(&page) && next != after) {
                thread::sleep(self.interval);
            }
            after = next;
        }
    }

    fn latest(&self) -> Result<u64, CliError> {
        let page = self
            .client
            .get("/api/v1/audit?after=0&limit=1", true)?
            .json()?;
        Ok(page
            .get("latestCursor")
            .and_then(Value::as_u64)
            .unwrap_or_default())
    }

    fn print(&mut self, records: &[Value]) -> Result<bool, CliError> {
        for record in records {
            match self.context.output() {
                Output::Json => self.context.print(record, View::Generic)?,
                _ => self
                    .context
                    .print_stream(&json!({ "records": [record] }), View::Audit)?,
            }
            self.remaining = self.remaining.map(|left| left - 1);
            if self.remaining == Some(0) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
