//! The statistics page of the stats page listener: one cached snapshot rendered as JSON, CSV and the
//! no-script table. The reference is "Stats page API" in `docs/OPERATIONS.md`.

mod admin_form;
mod builder;
mod cache;
#[cfg(test)]
mod contract;
mod csv;
mod html;
mod model;
mod page;

pub use page::HaproxyStatsPage;

#[cfg(test)]
mod tests;
