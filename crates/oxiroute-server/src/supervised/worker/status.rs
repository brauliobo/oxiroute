#[allow(clippy::wildcard_imports)]
use super::*;

/// How soon a change is looked for again after one was reported, and the minimum gap between two reports.
pub(super) const BUSY_TICK: Duration = Duration::from_millis(250);
/// How often a worker whose status did not change looks for a change.
pub(super) const IDLE_TICK: Duration = Duration::from_secs(1);

/// Builds the worker's status reports and sends them to the master.
///
/// Lifecycle changes always send one. The periodic check sends only when something the master can see
/// changed, so an idle worker is silent and costs only the counters it reads.
pub(super) struct StatusReporter<'a> {
    generation_id: GenerationId,
    generation: &'a RuntimeGeneration,
    manager: &'a GenerationManager,
    sequence: u64,
    event_cursor: u64,
    last: Option<WorkerStatus>,
    last_at: Instant,
}

impl<'a> StatusReporter<'a> {
    pub(super) fn new(
        generation_id: GenerationId,
        generation: &'a RuntimeGeneration,
        manager: &'a GenerationManager,
    ) -> Self {
        Self {
            generation_id,
            generation,
            manager,
            sequence: 1,
            event_cursor: 0,
            last: None,
            last_at: Instant::now(),
        }
    }

    pub(super) fn report(
        &mut self,
        control: &mut WorkerControl,
        lifecycle: WorkerLifecycle,
    ) -> Result<(), Box<dyn Error>> {
        let (status, event_cursor) = self.build(lifecycle);
        self.send(control, status, event_cursor)
    }

    /// Reports a change if one is due and returns how long the caller may sleep before checking again.
    ///
    /// Reports are at least `BUSY_TICK` apart; the next check is `BUSY_TICK` after a change and
    /// `IDLE_TICK` after a check that found none.
    pub(super) fn report_when_due(
        &mut self,
        control: &mut WorkerControl,
        lifecycle: WorkerLifecycle,
    ) -> Result<Duration, Box<dyn Error>> {
        if let Some(remaining) = BUSY_TICK
            .checked_sub(self.last_at.elapsed())
            .filter(|remaining| !remaining.is_zero())
        {
            return Ok(remaining);
        }
        let (status, event_cursor) = self.build(lifecycle);
        if !is_new_observation(self.last.as_ref(), &status) {
            return Ok(IDLE_TICK);
        }
        self.send(control, status, event_cursor)?;
        Ok(BUSY_TICK)
    }

    fn send(
        &mut self,
        control: &mut WorkerControl,
        status: WorkerStatus,
        event_cursor: u64,
    ) -> Result<(), Box<dyn Error>> {
        control.report_status(&status)?;
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| io::Error::other("worker status sequence exhausted"))?;
        self.event_cursor = event_cursor;
        self.last = Some(status);
        self.last_at = Instant::now();
        Ok(())
    }

    fn build(&self, lifecycle: WorkerLifecycle) -> (WorkerStatus, u64) {
        let generation = self.generation;
        let generation_status = self.manager.status();
        let report = generation.metrics().worker_report();
        let metrics = report
            .as_ref()
            .ok()
            .map(|report| metrics_status(&report.traffic));
        let listeners = report.as_ref().map_or_else(
            |_| Vec::new(),
            |report| listener_statuses(&report.listeners),
        );

        let event_page = worker_event_page(self.event_cursor, MAX_STATUS_EVENTS);
        let events = event_page
            .events
            .iter()
            .map(|event| WorkerEventRecord {
                cursor: event.cursor,
                timestamp_unix_ms: event.timestamp_unix_ms,
                event: event.event.as_str().to_owned(),
                outcome: event.outcome.as_str().to_owned(),
                revision: event.revision.as_ref().map(ToString::to_string),
                certificate: event.certificate.clone(),
                correlation_id: event.correlation_id.clone(),
                source: event.source.clone(),
                operation: event.operation.clone(),
            })
            .collect::<Vec<_>>();
        let next_event_cursor = if event_page.has_more {
            event_page
                .events
                .last()
                .map_or(self.event_cursor, |event| event.cursor)
        } else {
            event_page.latest_cursor
        };
        let metrics_degraded = report.is_err();
        let degraded = generation_status.degraded
            || generation.runtime_failed()
            || metrics_degraded
            || event_page.cursor_lost;
        let degradation = if metrics_degraded {
            Some("metrics_unavailable".to_owned())
        } else if event_page.cursor_lost {
            Some("event_cursor_lost".to_owned())
        } else if generation_status.degraded {
            generation_status
                .last_failure
                .or_else(|| {
                    generation_status
                        .last_rejection
                        .as_ref()
                        .map(|_| "config_rejected")
                })
                .map_or_else(
                    || Some("generation_degraded".to_owned()),
                    |value| Some(value.into()),
                )
        } else if generation.runtime_failed() {
            Some("runtime_failed".to_owned())
        } else {
            None
        };
        let administrative_state = report.as_ref().map_or_else(
            |_| match lifecycle {
                WorkerLifecycle::Quiescing
                | WorkerLifecycle::Draining
                | WorkerLifecycle::Stopping => WorkerAdministrativeState::Drain,
                _ => WorkerAdministrativeState::Ready,
            },
            |report| administrative_state(report.administrative_state),
        );
        let status = WorkerStatus {
            sequence: self.sequence,
            generation_id: self.generation_id,
            lifecycle,
            administrative_state,
            accepting: generation.accepting(),
            runtime_started: generation.runtime_started(),
            runtime_failed: generation.runtime_failed(),
            drained: generation.drained(),
            generation: generation_report(&generation_status),
            metrics,
            listeners,
            degraded,
            degradation,
            event_cursor: next_event_cursor,
            event_cursor_lost: event_page.cursor_lost,
            events,
        };
        (status, next_event_cursor)
    }
}

/// Whether `status` tells the master anything the last report did not; the sequence number does not count.
pub(super) fn is_new_observation(last: Option<&WorkerStatus>, status: &WorkerStatus) -> bool {
    last.is_none_or(|last| {
        WorkerStatus {
            sequence: last.sequence,
            ..status.clone()
        } != *last
    })
}

fn metrics_status(traffic: &TrafficSnapshot) -> WorkerMetrics {
    WorkerMetrics {
        accepted_connections: traffic.accepted_connections,
        rejected_connections: traffic.rejected_connections,
        active_connections: traffic.active_connections,
        bytes_received: traffic.bytes_received,
        bytes_sent: traffic.bytes_sent,
    }
}

pub(super) fn listener_statuses(
    listeners: &[oxiroute_server::ListenerSnapshot],
) -> Vec<WorkerListenerStatus> {
    listeners
        .iter()
        .map(|listener| WorkerListenerStatus {
            name: listener.name.clone(),
            protocol: listener.protocol.clone(),
            bind: listener.bind.clone(),
            administrative_state: administrative_state(listener.administrative_state),
            state: listener_state(listener.state),
            accepted_connections: listener.accepted_connections,
            rejected_connections: listener.rejected_connections,
            active_connections: listener.active_connections,
            bytes_received: listener.bytes_received,
            bytes_sent: listener.bytes_sent,
        })
        .collect()
}

const fn listener_state(state: ListenerRuntimeState) -> WorkerListenerState {
    match state {
        ListenerRuntimeState::Configured => WorkerListenerState::Configured,
        ListenerRuntimeState::Listening => WorkerListenerState::Listening,
        ListenerRuntimeState::Stopped => WorkerListenerState::Stopped,
        ListenerRuntimeState::Failed => WorkerListenerState::Failed,
    }
}

const fn administrative_state(state: AdministrativeState) -> WorkerAdministrativeState {
    match state {
        AdministrativeState::Ready => WorkerAdministrativeState::Ready,
        AdministrativeState::Drain => WorkerAdministrativeState::Drain,
        AdministrativeState::Maintenance => WorkerAdministrativeState::Maintenance,
    }
}

fn revision<T: ToString>(revision: Option<&T>) -> Option<String> {
    revision.map(ToString::to_string)
}

fn generation_report(status: &GenerationStatus) -> WorkerGenerationStatus {
    WorkerGenerationStatus {
        disk_revision: revision(status.disk_revision.as_ref()),
        candidate_revision: revision(status.candidate_revision.as_ref()),
        active_revision: revision(status.active_revision.as_ref()),
        previous_revision: revision(status.previous_revision.as_ref()),
        quarantined_revision: revision(status.quarantined_revision.as_ref()),
        active_accepting: status.active_accepting,
        degraded: status.degraded,
        last_failure: status.last_failure.map(str::to_owned),
        prepares: status.prepares,
        activations: status.activations,
        failures: status.failures,
        rollbacks: status.rollbacks,
    }
}
