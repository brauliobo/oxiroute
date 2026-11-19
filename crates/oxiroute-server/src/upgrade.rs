//! The binary hand-over request: how `oxiroute upgrade` reaches the master that serves a configuration.

mod client;
mod protocol;

pub use client::{UpgradeError, request};
pub use protocol::{Command, Event, control_address};

#[cfg(test)]
mod tests;
