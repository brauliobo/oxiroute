//! `oxiroute check`: one line and a Nagios exit code (0 OK, 1 WARNING, 2 CRITICAL, 3 UNKNOWN) for
//! monitoring systems and cron. It is the one command that does not use the 2..9 exit categories.

mod args;
mod run;

#[cfg(test)]
mod tests;

pub(super) use args::CheckArgs;
pub(super) use run::run;
