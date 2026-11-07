#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Debug)]
struct RetainedEvent {
    role: GenerationRole,
    event: AggregatedWorkerEvent,
}

#[derive(Debug, Default)]
pub(crate) struct SupervisorSnapshotHistory {
    metrics: WorkerMetrics,
    listeners: BTreeMap<(String, String, String), SupervisorListenerSnapshot>,
    events: VecDeque<RetainedEvent>,
    pub(super) next_event_cursor: u64,
}

impl SupervisorSnapshotHistory {
    pub(crate) fn new() -> Self {
        Self {
            next_event_cursor: 1,
            ..Self::default()
        }
    }

    pub(crate) fn record_events(
        &mut self,
        role: GenerationRole,
        instance_id: &InstanceId,
        generation_id: GenerationId,
        previous_cursor: u64,
        status: &WorkerStatus,
    ) -> u64 {
        let mut event_cursor = previous_cursor;
        for worker_event in &status.events {
            if worker_event.cursor <= event_cursor {
                continue;
            }
            let cursor = self.next_event_cursor;
            if cursor == 0 {
                break;
            }
            self.next_event_cursor = cursor.checked_add(1).unwrap_or(0);
            self.events.push_back(RetainedEvent {
                role,
                event: AggregatedWorkerEvent {
                    cursor,
                    instance_id: instance_id.clone(),
                    generation_id,
                    worker_event: worker_event.clone(),
                },
            });
            while self.events.len() > MAX_AGGREGATED_EVENTS {
                self.events.pop_front();
            }
            event_cursor = worker_event.cursor;
        }
        event_cursor.max(status.event_cursor)
    }

    pub(crate) fn reap(&mut self, status: Option<&WorkerStatus>) {
        let Some(status) = status else {
            return;
        };
        if let Some(metrics) = status.metrics {
            add_cumulative_metrics(&mut self.metrics, metrics);
        }
        for listener in &status.listeners {
            let entry = self
                .listeners
                .entry(listener_key(listener))
                .or_insert_with(|| listener_snapshot(listener));
            add_cumulative_listener(entry, listener);
            entry.active_connections = 0;
        }
    }

    pub(crate) fn worker_events(&self, after: u64, limit: usize) -> Vec<AggregatedWorkerEvent> {
        self.events
            .iter()
            .filter(|retained| retained.event.cursor > after)
            .take(limit.min(MAX_AGGREGATED_EVENTS))
            .map(|retained| retained.event.clone())
            .collect()
    }

    pub(crate) fn snapshot<R: Clone, T>(
        &self,
        catalog: &SupervisedGenerationCatalog<R, T>,
        sources: &[WorkerSnapshotSource<'_>],
    ) -> Result<SupervisorSnapshot<R>, SupervisorSnapshotError> {
        let generations = catalog
            .documents()
            .map(|(role, document)| SupervisorGenerationSnapshot {
                role,
                instance_id: document.instance_id().clone(),
                generation_id: document.generation_id(),
                revision: document.revision().clone(),
            })
            .collect::<Vec<_>>();
        for source in sources {
            let catalog_role = catalog.documents().find_map(|(role, document)| {
                (document.instance_id() == source.instance_id
                    && document.generation_id() == source.generation_id)
                    .then_some(role)
            });
            if catalog_role != Some(source.role) {
                return Err(SupervisorSnapshotError {
                    instance_id: source.instance_id.clone(),
                    observed_role: source.role,
                    catalog_role,
                });
            }
        }

        let mut metrics = self.metrics;
        let mut listeners = self.listeners.clone();
        let mut listener_priorities = BTreeMap::new();
        let mut processes = Vec::with_capacity(sources.len());
        let mut listener_observations = Vec::new();
        let mut degradation = Vec::new();
        for source in sources {
            let status = source.status;
            if let Some(worker_metrics) = status.metrics {
                add_all_metrics(&mut metrics, worker_metrics);
            }
            processes.push(SupervisorProcessSnapshot {
                role: source.role,
                instance_id: source.instance_id.clone(),
                generation_id: source.generation_id,
                sequence: status.sequence,
                lifecycle: status.lifecycle,
                administrative_state: status.administrative_state,
                accepting: status.accepting,
                runtime_started: status.runtime_started,
                runtime_failed: status.runtime_failed,
                drained: status.drained,
                generation: status.generation.clone(),
            });
            for listener in &status.listeners {
                listener_observations.push(SupervisorListenerObservation {
                    role: source.role,
                    instance_id: source.instance_id.clone(),
                    listener: listener.clone(),
                });
                let key = listener_key(listener);
                let priority = role_priority(source.role);
                let entry = listeners
                    .entry(key.clone())
                    .or_insert_with(|| listener_snapshot(listener));
                add_all_listener(entry, listener);
                if listener_priorities
                    .get(&key)
                    .is_none_or(|current| priority > *current)
                {
                    entry.administrative_state = listener.administrative_state;
                    entry.state = listener.state;
                    listener_priorities.insert(key, priority);
                }
            }
            if status.degraded || status.generation.degraded || status.runtime_failed {
                degradation.push(SupervisorDegradation {
                    role: source.role,
                    instance_id: source.instance_id.clone(),
                    reason: status
                        .degradation
                        .clone()
                        .or_else(|| status.generation.last_failure.clone()),
                });
            }
        }
        let events = self
            .events
            .iter()
            .map(|retained| SupervisorEventRecord {
                role: retained.role,
                event: retained.event.clone(),
            })
            .collect();
        Ok(SupervisorSnapshot {
            generations,
            processes,
            listener_observations,
            listeners: listeners.into_values().collect(),
            degraded: !degradation.is_empty(),
            degradation,
            metrics,
            events,
        })
    }
}

pub(crate) fn retain_monotonic_status(previous: &WorkerStatus, status: &mut WorkerStatus) {
    match (&previous.metrics, &mut status.metrics) {
        (Some(previous), Some(current)) => retain_monotonic_metrics(*previous, current),
        (Some(previous), None) => status.metrics = Some(*previous),
        (None, _) => {}
    }
    status.generation.prepares = status.generation.prepares.max(previous.generation.prepares);
    status.generation.activations = status
        .generation
        .activations
        .max(previous.generation.activations);
    status.generation.failures = status.generation.failures.max(previous.generation.failures);
    status.generation.rollbacks = status
        .generation
        .rollbacks
        .max(previous.generation.rollbacks);
    for previous_listener in &previous.listeners {
        if let Some(current) = status.listeners.iter_mut().find(|current| {
            current.name == previous_listener.name
                && current.protocol == previous_listener.protocol
                && current.bind == previous_listener.bind
        }) {
            retain_monotonic_listener(previous_listener, current);
        } else {
            status.listeners.push(previous_listener.clone());
        }
    }
}

fn listener_key(listener: &WorkerListenerStatus) -> (String, String, String) {
    (
        listener.name.clone(),
        listener.protocol.clone(),
        listener.bind.clone(),
    )
}

fn listener_snapshot(listener: &WorkerListenerStatus) -> SupervisorListenerSnapshot {
    SupervisorListenerSnapshot {
        name: listener.name.clone(),
        protocol: listener.protocol.clone(),
        bind: listener.bind.clone(),
        administrative_state: listener.administrative_state,
        state: listener.state,
        accepted_connections: 0,
        rejected_connections: 0,
        active_connections: 0,
        bytes_received: 0,
        bytes_sent: 0,
    }
}

fn add_cumulative_metrics(total: &mut WorkerMetrics, value: WorkerMetrics) {
    total.accepted_connections = total
        .accepted_connections
        .saturating_add(value.accepted_connections);
    total.rejected_connections = total
        .rejected_connections
        .saturating_add(value.rejected_connections);
    total.bytes_received = total.bytes_received.saturating_add(value.bytes_received);
    total.bytes_sent = total.bytes_sent.saturating_add(value.bytes_sent);
}

fn add_all_metrics(total: &mut WorkerMetrics, value: WorkerMetrics) {
    add_cumulative_metrics(total, value);
    total.active_connections = total
        .active_connections
        .saturating_add(value.active_connections);
}

fn retain_monotonic_metrics(previous: WorkerMetrics, current: &mut WorkerMetrics) {
    current.accepted_connections = current
        .accepted_connections
        .max(previous.accepted_connections);
    current.rejected_connections = current
        .rejected_connections
        .max(previous.rejected_connections);
    current.bytes_received = current.bytes_received.max(previous.bytes_received);
    current.bytes_sent = current.bytes_sent.max(previous.bytes_sent);
}

fn add_cumulative_listener(total: &mut SupervisorListenerSnapshot, value: &WorkerListenerStatus) {
    total.accepted_connections = total
        .accepted_connections
        .saturating_add(value.accepted_connections);
    total.rejected_connections = total
        .rejected_connections
        .saturating_add(value.rejected_connections);
    total.bytes_received = total.bytes_received.saturating_add(value.bytes_received);
    total.bytes_sent = total.bytes_sent.saturating_add(value.bytes_sent);
}

fn add_all_listener(total: &mut SupervisorListenerSnapshot, value: &WorkerListenerStatus) {
    add_cumulative_listener(total, value);
    total.active_connections = total
        .active_connections
        .saturating_add(value.active_connections);
}

fn retain_monotonic_listener(previous: &WorkerListenerStatus, current: &mut WorkerListenerStatus) {
    current.accepted_connections = current
        .accepted_connections
        .max(previous.accepted_connections);
    current.rejected_connections = current
        .rejected_connections
        .max(previous.rejected_connections);
    current.bytes_received = current.bytes_received.max(previous.bytes_received);
    current.bytes_sent = current.bytes_sent.max(previous.bytes_sent);
}

const fn role_priority(role: GenerationRole) -> u8 {
    match role {
        GenerationRole::Active => 5,
        GenerationRole::Candidate => 4,
        GenerationRole::Previous => 3,
        GenerationRole::Quarantined => 2,
        GenerationRole::RestartRequired => 1,
    }
}
