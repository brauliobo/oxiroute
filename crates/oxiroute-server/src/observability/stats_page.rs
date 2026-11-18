//! The statistics page of the stats page listener: one cached snapshot rendered as JSON, CSV and the
//! no-script table. The reference is "Stats page API" in `docs/OPERATIONS.md`.

mod admin_form;
mod assets;
mod builder;
mod cache;
#[cfg(test)]
mod contract;
mod csv;
mod html;
pub(crate) mod model;
mod page;

pub use page::HaproxyStatsPage;

pub(crate) fn management_document(
    generations: &crate::GenerationManager,
) -> Result<Vec<u8>, builder::BuildError> {
    let snapshot = builder::build_snapshot(generations, builder::MIN_INTERVAL_MS)?;
    let document = model::StatsDocument {
        admin: false,
        snapshot: &snapshot,
    };
    Ok(serde_json::to_vec(&document).expect("snapshot serialises"))
}

#[cfg(test)]
mod tests;
