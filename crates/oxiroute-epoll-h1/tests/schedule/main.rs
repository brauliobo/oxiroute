#![allow(clippy::pedantic)]
//! The engine with `scheduling` on: the order ready connections are served in, where new connections go, and how the
//! active set of a scaled engine follows the load while connections keep their data. Real sockets, a real origin.

#[path = "../support/mod.rs"]
mod support;

mod common;
mod drain;
mod migration;
mod order;
mod placement;
mod scaling;
