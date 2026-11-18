//! `oxiroute top`: a live, read-only dashboard of listeners, pools, servers and in-flight requests, or one
//! snapshot for scripts. It reads `GET /api/v1/stats` and `GET /api/v1/requests` and computes rates itself.

mod app;
mod args;
mod draw;
mod keys;
mod once;
mod rates;
mod rows;
mod run;
mod session;
mod source;
mod summary;

#[cfg(test)]
mod tests;

pub(super) use args::TopArgs;
