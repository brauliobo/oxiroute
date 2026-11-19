//! `oxiroute top`: a live, read-only dashboard of listeners, upstreams, clients, connections, in-flight requests,
//! RTMP sessions and generations, or one measured snapshot as text, JSON or CSV for scripts.

mod app;
mod args;
mod draw;
mod export;
mod headline;
mod history;
mod layout;
mod model;
mod prefs;
mod run;
mod scene;
mod session;
mod source;
mod theme;
mod totals;
mod views;

#[cfg(test)]
mod tests;

pub(super) use args::TopArgs;
