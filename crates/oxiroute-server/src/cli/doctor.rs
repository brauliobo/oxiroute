//! `oxiroute doctor`: one pass over what keeps a daemon from starting or serving, as a prioritised report and a
//! Nagios exit code (0 OK, 1 WARNING, 2 CRITICAL, 3 UNKNOWN) like the other monitoring commands.

mod args;
mod certificates;
mod config;
mod contexts;
mod diagnosis;
mod files;
mod listeners;
mod run;
mod sidecars;
mod unit;
mod upstreams;
mod writes;

#[cfg(test)]
mod tests;

pub(super) use args::DoctorArgs;
pub(super) use run::run;
