use std::fmt;

use super::format::{labels, sample};
use crate::registry::service::RegistryMetrics;

/// Registry series. Worker names label the lease gauge, which `max_servers` bounds per pool.
#[allow(clippy::cast_precision_loss)]
pub(super) fn render_registry(output: &mut String, metrics: &RegistryMetrics) -> fmt::Result {
    for ((pool, state), count) in &metrics.servers {
        labels(
            output,
            "oxiroute_registry_servers",
            &[("pool", pool), ("state", state)],
            count,
        )?;
    }
    for ((operation, result), count) in &metrics.operations {
        labels(
            output,
            "oxiroute_registry_operations_total",
            &[("op", operation), ("result", result)],
            count,
        )?;
    }
    for (pool, count) in &metrics.expirations {
        labels(
            output,
            "oxiroute_registry_expirations_total",
            &[("pool", pool)],
            count,
        )?;
    }
    for (reason, count) in &metrics.rejections {
        labels(
            output,
            "oxiroute_registry_rejected_total",
            &[("reason", reason)],
            count,
        )?;
    }
    for ((pool, server), left) in &metrics.lease_remaining_ms {
        labels(
            output,
            "oxiroute_registry_lease_remaining_seconds",
            &[("pool", pool), ("server", server)],
            *left as f64 / 1_000.0,
        )?;
    }
    sample(
        output,
        "oxiroute_registry_store_errors_total",
        metrics.store_errors,
    )?;
    sample(output, "oxiroute_registry_revision", metrics.revision)
}
