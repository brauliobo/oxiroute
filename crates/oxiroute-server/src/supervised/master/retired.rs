#[allow(clippy::wildcard_imports)]
use super::*;

/// How often the active worker is told what the retired worker is doing.
const RETIRED_REPORT_INTERVAL: Duration = Duration::from_secs(1);

/// Tells the active worker about the worker a replacement retired, so that it can list it in
/// `generation status`.
#[derive(Default)]
pub(super) struct RetiredReports {
    deadline: Duration,
    since: Option<Instant>,
    pub(super) last_sent: Option<Instant>,
    listed: bool,
}

impl RetiredReports {
    /// A replacement is starting; its retired worker may drain for `deadline`.
    pub(super) fn begin(&mut self, deadline: Duration) {
        self.deadline = deadline;
    }

    /// When the worker the master retired is next due a report; `None` while there is nothing to wait for.
    pub(super) fn next_due(&self) -> Option<Instant> {
        self.last_sent.map(|sent| sent + RETIRED_REPORT_INTERVAL)
    }

    pub(super) fn report(&mut self, master: &mut Master, now: Instant) {
        if master.worker_id(WorkerRole::Retired).is_none() {
            self.since = None;
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
        let since = *self.since.get_or_insert(now);
        if self
            .last_sent
            .is_some_and(|sent| now.duration_since(sent) < RETIRED_REPORT_INTERVAL)
        {
            return;
        }
        let Some(report) = master
            .worker_status(WorkerRole::Retired)
            .and_then(|status| remote_retired(status, now.duration_since(since), self.deadline))
        else {
            return;
        };
        self.last_sent = Some(now);
        self.listed = true;
        send_notice(
            master,
            &WorkerNotice::Retired {
                generations: vec![report],
            },
        );
    }
}

/// The retired worker as the master observes it: its revision and connection total.
pub(super) fn remote_retired(
    status: &WorkerStatus,
    age: Duration,
    deadline: Duration,
) -> Option<RemoteRetired> {
    Some(RemoteRetired {
        revision: status.generation.active_revision.clone()?,
        age_seconds: age.as_secs(),
        deadline_remaining_seconds: deadline.saturating_sub(age).as_secs(),
        connections: status
            .metrics
            .map_or(0, |metrics| metrics.active_connections),
    })
}
