mod plan;

pub(crate) use plan::{EnginePlan, plan};

#[cfg(target_os = "linux")]
mod exchange;
#[cfg(target_os = "linux")]
mod handoff;
#[cfg(target_os = "linux")]
mod lease;
#[cfg(target_os = "linux")]
mod outcome;
#[cfg(target_os = "linux")]
mod render;
#[cfg(target_os = "linux")]
mod routes;
#[cfg(target_os = "linux")]
mod runtime;
#[cfg(target_os = "linux")]
mod service;

#[cfg(target_os = "linux")]
pub use runtime::{EpollHttpService, EpollLane};

/// The log target of the debug line that says a request was served by the epoll data plane, with its id: how a test
/// tells the plane from the tokio path.
#[cfg(target_os = "linux")]
pub(crate) const LOG_TARGET: &str = "oxiroute::epoll";
