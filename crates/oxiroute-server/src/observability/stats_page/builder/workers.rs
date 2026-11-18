use oxiroute_config::ConfigDraft;

use super::super::model::{RuntimeWorkers, WorkerScaling, WorkerStats};

pub(super) fn workers(config: &ConfigDraft) -> WorkerStats {
    let dynamic = config.dynamic_worker_scaling();
    WorkerStats {
        scaling: if dynamic {
            WorkerScaling::Dynamic
        } else {
            WorkerScaling::Static
        },
        runtimes: if dynamic { runtimes() } else { Vec::new() },
    }
}

fn runtimes() -> Vec<RuntimeWorkers> {
    oxiroute_worker_scaling::process_stats()
        .into_iter()
        .map(|(runtime, stats)| RuntimeWorkers {
            runtime,
            workers: stats.workers as u64,
            active: stats.active as u64,
            relocations: stats.relocations.into(),
        })
        .collect()
}
