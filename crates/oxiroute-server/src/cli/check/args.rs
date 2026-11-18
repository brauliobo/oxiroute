use clap::Args;

use crate::{
    cli::parse::{nonempty_target, parse_age_ms},
    observability::status::check::Thresholds,
};

#[derive(Args)]
pub(crate) struct CheckArgs {
    /// Judge only this pool; repeat for several. Default: every pool.
    #[arg(long = "pool", value_parser = nonempty_target)]
    pub(super) pools: Vec<String>,
    /// WARNING when a pool has fewer available servers than this. Default: any server missing.
    #[arg(long, value_name = "N")]
    pub(super) warn_available_below: Option<u16>,
    /// CRITICAL when a pool has fewer available servers than this. Default: 1, except that a pool
    /// that takes registrations and has no servers yet is only noted unless a threshold is given.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u16).range(1..))]
    pub(super) crit_available_below: Option<u16>,
    /// WARNING when the oldest queued request has waited longer, for example 5s.
    #[arg(long, value_name = "AGE", default_value = "5s", value_parser = parse_age_ms)]
    pub(super) max_queue_wait: u64,
    /// WARNING when a pool holds a request older than this, for example 60s. Default: not judged.
    #[arg(long, value_name = "AGE", value_parser = parse_age_ms)]
    pub(super) max_inflight_age: Option<u64>,
    /// WARNING when a retired generation still drains after this long, for example 1h.
    #[arg(long, value_name = "AGE", default_value = "1h", value_parser = parse_age_ms)]
    pub(super) max_retired_age: u64,
    /// Print one JSON object instead of the plugin line.
    #[arg(long)]
    pub(super) json: bool,
    /// Also request `--url` the way a client would and require `--expect-status`.
    #[arg(long, requires = "url")]
    pub(super) canary: bool,
    /// Plain `http://host:port/path` to request for `--canary`.
    #[arg(long, value_name = "URL", requires = "canary")]
    pub(super) url: Option<String>,
    /// Host header for `--canary`; default: the authority of `--url`.
    #[arg(long, value_name = "HOST", requires = "canary")]
    pub(super) host: Option<String>,
    /// Status `--canary` must answer.
    #[arg(
        long,
        value_name = "STATUS",
        default_value_t = 200,
        requires = "canary"
    )]
    pub(super) expect_status: u16,
    /// WARNING when the `--canary` answer takes longer, for example 2s. Default: not judged.
    #[arg(long, value_name = "AGE", value_parser = parse_age_ms, requires = "canary")]
    pub(super) canary_max_latency: Option<u64>,
}

impl CheckArgs {
    pub(super) fn thresholds(&self) -> Thresholds {
        Thresholds {
            warn_available_below: self.warn_available_below.map(usize::from),
            crit_available_below: self.crit_available_below.map(usize::from),
            max_queue_wait_ms: self.max_queue_wait,
            max_inflight_age_ms: self.max_inflight_age,
            max_retired_age_seconds: self.max_retired_age / 1_000,
        }
    }
}
