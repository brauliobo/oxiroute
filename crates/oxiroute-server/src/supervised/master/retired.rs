#[allow(clippy::wildcard_imports)]
use super::*;

/// How often the active worker is told what the retired workers are doing.
const RETIRED_REPORT_INTERVAL: Duration = Duration::from_secs(1);

/// Tells the active worker about the workers the replacements retired, so that it can list them in
/// `generation status`.
#[derive(Default)]
pub(super) struct RetiredReports {
    pub(super) last_sent: Option<Instant>,
    listed: bool,
}

impl RetiredReports {
    /// When the workers the master retired are next due a report; `None` while there is nothing to wait for.
    pub(super) fn next_due(&self) -> Option<Instant> {
        self.last_sent.map(|sent| sent + RETIRED_REPORT_INTERVAL)
    }

    /// Makes the next report due at once when the events changed what is retired.
    pub(super) fn observe(&mut self, events: &[MasterEvent]) {
        let changed = events.iter().any(|event| {
            matches!(
                event,
                MasterEvent::RetiredForced { .. }
                    | MasterEvent::ReplacementCommitted { .. }
                    | MasterEvent::WorkerExited {
                        role: WorkerRole::Retired,
                        ..
                    }
            )
        });
        if changed {
            self.last_sent = None;
        }
    }

    pub(super) fn report(&mut self, master: &mut Master, now: Instant) {
        let retired = master.retired_workers();
        if retired.is_empty() {
            self.last_sent = None;
            if std::mem::take(&mut self.listed) {
                send_notice(
                    master,
                    &WorkerNotice::Retired {
                        generations: Vec::new(),
                    },
                );
            }
            return;
        }
        if self
            .last_sent
            .is_some_and(|sent| now.duration_since(sent) < RETIRED_REPORT_INTERVAL)
        {
            return;
        }
        let generations = retired
            .iter()
            .filter_map(|worker| remote_retired(worker, now))
            .collect();
        self.last_sent = Some(now);
        self.listed = true;
        send_notice(master, &WorkerNotice::Retired { generations });
    }
}

/// A retired worker as the master observes it: its revision and connection total.
pub(super) fn remote_retired(worker: &RetiredWorker<'_>, now: Instant) -> Option<RemoteRetired> {
    let age = now.saturating_duration_since(worker.since);
    let deadline = worker.window.saturating_sub(RETIREMENT_BACKSTOP);
    Some(RemoteRetired {
        revision: worker.status?.generation.active_revision.clone()?,
        age_seconds: age.as_secs(),
        deadline_remaining_seconds: deadline.saturating_sub(age).as_secs(),
        connections: worker
            .status
            .and_then(|status| status.metrics)
            .map_or(0, |metrics| metrics.active_connections),
        forced: worker.forced.map(|reason| reason.as_str().to_owned()),
    })
}
