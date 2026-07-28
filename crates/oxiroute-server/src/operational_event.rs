use std::{
    collections::VecDeque,
    sync::{Mutex, OnceLock},
    time::SystemTime,
};

use serde::Serialize;

use crate::config_coordinator::ConfigRevision;

const EVENT_CAPACITY: usize = 2_048;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OperationalEvent {
    pub cursor: u64,
    pub timestamp_unix_ms: Option<u64>,
    pub event: String,
    pub outcome: String,
    pub revision: Option<ConfigRevision>,
}

#[derive(Default)]
struct EventLog {
    next_cursor: u64,
    events: VecDeque<OperationalEvent>,
}

fn log() -> &'static Mutex<EventLog> {
    static LOG: OnceLock<Mutex<EventLog>> = OnceLock::new();
    LOG.get_or_init(|| Mutex::new(EventLog::default()))
}

pub(crate) fn emit(event: &str, outcome: &str, revision: Option<&ConfigRevision>) {
    let mut state = log()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.next_cursor = state.next_cursor.saturating_add(1);
    let value = OperationalEvent {
        cursor: state.next_cursor,
        timestamp_unix_ms: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_millis()).ok()),
        event: event.to_owned(),
        outcome: outcome.to_owned(),
        revision: revision.cloned(),
    };
    if state.events.len() == EVENT_CAPACITY {
        state.events.pop_front();
    }
    state.events.push_back(value.clone());
    drop(state);
    if let Ok(json) = serde_json::to_string(&value) {
        log::info!(target: "oxiroute::operations", "{json}");
    }
}

pub(crate) fn list(after: u64, limit: usize) -> (Vec<OperationalEvent>, u64, bool, Option<u64>) {
    let state = log()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.list(after, limit)
}

impl EventLog {
    fn list(&self, after: u64, limit: usize) -> (Vec<OperationalEvent>, u64, bool, Option<u64>) {
        let events: Vec<_> = self
            .events
            .iter()
            .filter(|event| event.cursor > after)
            .take(limit.min(EVENT_CAPACITY))
            .cloned()
            .collect();
        let cursor = events.last().map_or(after, |event| event.cursor);
        let has_more = self.events.iter().any(|event| event.cursor > cursor);
        let oldest_cursor = self.events.front().map(|event| event.cursor);
        (events, cursor, has_more, oldest_cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_pages_advance_to_the_last_returned_event() {
        let mut log = EventLog::default();
        for cursor in 1..=5 {
            log.events.push_back(OperationalEvent {
                cursor,
                timestamp_unix_ms: None,
                event: "test".into(),
                outcome: "ok".into(),
                revision: None,
            });
            log.next_cursor = cursor;
        }

        let (first, cursor, has_more, oldest) = log.list(0, 2);
        assert_eq!(
            first.iter().map(|event| event.cursor).collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(cursor, 2);
        assert!(has_more);
        assert_eq!(oldest, Some(1));

        let (second, cursor, has_more, _) = log.list(cursor, 2);
        assert_eq!(
            second.iter().map(|event| event.cursor).collect::<Vec<_>>(),
            [3, 4]
        );
        assert_eq!(cursor, 4);
        assert!(has_more);
    }
}
