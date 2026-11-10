//! Keeps a listener's connections on as few of its workers as the load needs.
//!
//! A listener with many workers and a light load pays for it: every worker wakes for a handful of events, the kernel is
//! entered once per event and each request costs a thread wake-up. Packing the connections onto fewer workers
//! costs a share of the tail latency and saves CPU, but a fixed packing loses the rate when the load rises. This crate
//! sizes the packing by the load instead.
//!
//! * [`Controller`] is the pure decision: the busy share of every worker in, the number of active workers out, with
//!   hysteresis (activate at once, release only after the load has fitted fewer workers for a delay).
//! * [`Scaling`] is the state of one listener runtime: the active count, the placement of new connections on the active
//!   workers, the relocation of idle connections off released or crowded workers, and the controller thread that reads
//!   the workers through [`Workers`] every tick and sleeps while no connection exists.
//! * [`process_stats`] lists the live runtimes for metrics.

mod controller;
mod scaling;

pub use controller::{Controller, Policy};
pub use scaling::{Scaling, Stats, Workers, process_stats};
