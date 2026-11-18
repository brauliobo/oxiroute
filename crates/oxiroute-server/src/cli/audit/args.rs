use std::time::Duration;

use clap::{Args, Subcommand, ValueEnum};

use crate::cli::parse::{parse_duration, parse_timeout};

#[derive(Clone, Copy, ValueEnum)]
pub(super) enum Category {
    Reload,
    Import,
    Certificate,
    Control,
}

#[derive(Clone, Copy, ValueEnum)]
pub(super) enum Outcome {
    Requested,
    Succeeded,
    Failed,
    Rejected,
    Conflict,
    Partial,
    Degraded,
}

#[derive(Args)]
pub(crate) struct Filter {
    /// Only records of this category.
    #[arg(long, value_enum)]
    pub(super) category: Option<Category>,
    /// Only records with this result.
    #[arg(long, value_enum)]
    pub(super) result: Option<Outcome>,
}

#[derive(Subcommand)]
pub(crate) enum AuditCommand {
    /// List the audit records of administrative actions, oldest first.
    List {
        /// Only records after this cursor; 0 starts at the oldest retained record.
        #[arg(long, default_value_t = 0, conflicts_with = "since")]
        after: u64,
        /// Most records to print, 1 to 1000.
        #[arg(long, default_value_t = 100, conflicts_with = "since", value_parser = clap::value_parser!(u16).range(1..=1000))]
        limit: u16,
        /// Every retained record of the last DURATION instead of a page, for example 1h.
        #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
        since: Option<Duration>,
        #[command(flatten)]
        filter: Filter,
    },
    /// Show whether the audit log is persistent and healthy, with its size and limits.
    Status,
    /// Print new audit records as they are written until interrupted.
    ///
    /// Asks the daemon for new records every --interval; Ctrl-C ends it with status 130.
    Tail {
        /// Print the retained records after this cursor first; 0 starts at the oldest retained record.
        #[arg(long, conflicts_with = "since")]
        after: Option<u64>,
        /// Print the retained records of the last DURATION first, for example 1h.
        #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
        since: Option<Duration>,
        /// Time between asking for new records.
        #[arg(long, value_name = "DURATION", default_value = "1s", value_parser = parse_timeout)]
        interval: Duration,
        /// Stop after printing this many records instead of waiting for Ctrl-C.
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
        count: Option<u64>,
        #[command(flatten)]
        filter: Filter,
    },
}
