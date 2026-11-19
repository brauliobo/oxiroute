use log::info;
use oxiroute_config::ValidatedConfig;

use super::log_filter::STARTUP_LOG_TARGET;
use crate::{
    config::coordinator::EffectiveRevision,
    listener::inventory::{ListenerInventory, ListenerPlane},
};

const REVISION_PREFIX_CHARACTERS: usize = 12;

/// Says which instruction-set level the byte scanners of this process run at.
pub fn announce_cpu_level() {
    info!(
        target: STARTUP_LOG_TARGET,
        "instruction-set level {}", oxiroute_cpu::level().name()
    );
}

/// Says what a started process serves: one line per listener, the management URL and a ready line.
///
/// The lines go to the startup log target, so a `log_filter` or `RUST_LOG` that excludes it silences them.
pub fn announce_ready(config: &ValidatedConfig, revision: &EffectiveRevision) {
    let draft = config.as_draft();
    let inventory = ListenerInventory::compile(config);
    let traffic = inventory
        .entries()
        .iter()
        .filter(|entry| entry.plane == ListenerPlane::Data);
    let mut listeners = 0_usize;
    for entry in traffic {
        listeners += 1;
        info!(
            target: STARTUP_LOG_TARGET,
            "listening on {} ({}, {})", entry.bind, entry.name, entry.protocol_name()
        );
    }
    if let Some(management) = &draft.management {
        let console = if management.ui_dir.is_some() {
            " (API and console)"
        } else {
            " (API)"
        };
        info!(target: STARTUP_LOG_TARGET, "management at http://{}/{console}", management.bind);
    }
    if let Some(stats) = &draft.stats {
        for bind in &stats.binds {
            info!(target: STARTUP_LOG_TARGET, "statistics API at http://{bind}/stats");
        }
        for page in &stats.pages {
            info!(
                target: STARTUP_LOG_TARGET,
                "statistics page at http://{}{}", page.bind, page.uri_prefix
            );
        }
    }
    announce_cpu_level();
    let prefix = revision
        .as_str()
        .chars()
        .take(REVISION_PREFIX_CHARACTERS)
        .collect::<String>();
    info!(
        target: STARTUP_LOG_TARGET,
        "OxiRoute {} ready, listeners: {listeners}, configuration revision {prefix}",
        crate::cli::BUILD_VERSION
    );
}
