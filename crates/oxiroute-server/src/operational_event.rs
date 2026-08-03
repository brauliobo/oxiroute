use std::{
    collections::VecDeque,
    sync::{Mutex, OnceLock},
    time::SystemTime,
};

use serde::Serialize;
use tokio::sync::Notify;

use crate::config_coordinator::ConfigRevision;

pub(crate) const EVENT_CAPACITY: usize = 2_048;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OperationalEvent {
    pub cursor: u64,
    pub timestamp_unix_ms: Option<u64>,
    pub event: EventName,
    pub outcome: EventOutcome,
    pub revision: Option<ConfigRevision>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EventName {
    GenerationPrepare,
    GenerationActivate,
    GenerationRollback,
    GenerationStart,
    ProcessShutdown,
    ListenerAdministrativeState,
    PoolAdministrativeState,
    ServerUpdate,
    Unknown,
}

impl EventName {
    fn parse(value: &str) -> Self {
        match value {
            "generation_prepare" => Self::GenerationPrepare,
            "generation_activate" => Self::GenerationActivate,
            "generation_rollback" => Self::GenerationRollback,
            "generation_start" => Self::GenerationStart,
            "process_shutdown" => Self::ProcessShutdown,
            "listener_administrative_state" => Self::ListenerAdministrativeState,
            "pool_administrative_state" => Self::PoolAdministrativeState,
            "server_update" => Self::ServerUpdate,
            _ => Self::Unknown,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::GenerationPrepare => "generation_prepare",
            Self::GenerationActivate => "generation_activate",
            Self::GenerationRollback => "generation_rollback",
            Self::GenerationStart => "generation_start",
            Self::ProcessShutdown => "process_shutdown",
            Self::ListenerAdministrativeState => "listener_administrative_state",
            Self::PoolAdministrativeState => "pool_administrative_state",
            Self::ServerUpdate => "server_update",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EventOutcome {
    Prepared,
    Rejected,
    Activated,
    Quarantined,
    Requested,
    Applied,
    Unknown,
}

impl EventOutcome {
    fn parse(value: &str) -> Self {
        match value {
            "prepared" => Self::Prepared,
            "rejected" => Self::Rejected,
            "activated" => Self::Activated,
            "quarantined" => Self::Quarantined,
            "requested" => Self::Requested,
            "applied" => Self::Applied,
            _ => Self::Unknown,
        }
    }
}

pub(crate) struct EventPage {
    pub events: Vec<OperationalEvent>,
    pub cursor: u64,
    pub has_more: bool,
    pub oldest_cursor: Option<u64>,
    pub latest_cursor: u64,
    pub cursor_lost: bool,
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

fn notifications() -> &'static Notify {
    static NOTIFICATIONS: OnceLock<Notify> = OnceLock::new();
    NOTIFICATIONS.get_or_init(Notify::new)
}

pub(crate) fn emit(event: &str, outcome: &str, revision: Option<&ConfigRevision>) {
    let mut state = log()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.next_cursor = state.next_cursor.saturating_add(1);
    let value = OperationalEvent::new(
        state.next_cursor,
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_millis()).ok()),
        event,
        outcome,
        revision,
    );
    if state.events.len() == EVENT_CAPACITY {
        state.events.pop_front();
    }
    state.events.push_back(value.clone());
    drop(state);
    if let Ok(json) = serde_json::to_string(&value) {
        log::info!(target: "oxiroute::operations", "{json}");
    }
    notifications().notify_one();
}

pub(crate) fn list(after: u64, limit: usize) -> (Vec<OperationalEvent>, u64, bool, Option<u64>) {
    let state = log()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let page = state.page(after, limit);
    (page.events, page.cursor, page.has_more, page.oldest_cursor)
}

pub(crate) fn page(after: u64, limit: usize) -> EventPage {
    let state = log()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.page(after, limit)
}

pub(crate) fn current_cursor() -> u64 {
    log()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .next_cursor
}

pub(crate) async fn wait_for_event() {
    notifications().notified().await;
}

impl EventLog {
    fn page(&self, after: u64, limit: usize) -> EventPage {
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
        EventPage {
            events,
            cursor,
            has_more,
            oldest_cursor,
            latest_cursor: self.next_cursor,
            cursor_lost: oldest_cursor.is_some_and(|oldest| after < oldest.saturating_sub(1)),
        }
    }
}

impl OperationalEvent {
    fn new(
        cursor: u64,
        timestamp_unix_ms: Option<u64>,
        event: &str,
        outcome: &str,
        revision: Option<&ConfigRevision>,
    ) -> Self {
        Self {
            cursor,
            timestamp_unix_ms,
            event: EventName::parse(event),
            outcome: EventOutcome::parse(outcome),
            revision: revision.cloned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_pages_advance_to_the_last_returned_event() {
        let mut log = EventLog::default();
        for cursor in 1..=5 {
            log.events
                .push_back(OperationalEvent::new(cursor, None, "test", "ok", None));
            log.next_cursor = cursor;
        }

        let first = log.page(0, 2);
        assert_eq!(
            first
                .events
                .iter()
                .map(|event| event.cursor)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(first.cursor, 2);
        assert!(first.has_more);
        assert_eq!(first.oldest_cursor, Some(1));
        assert!(!first.cursor_lost);

        let second = log.page(first.cursor, 2);
        assert_eq!(
            second
                .events
                .iter()
                .map(|event| event.cursor)
                .collect::<Vec<_>>(),
            [3, 4]
        );
        assert_eq!(second.cursor, 4);
        assert!(second.has_more);
        assert_eq!(second.latest_cursor, 5);
    }

    #[test]
    fn an_initial_page_uses_the_current_cursor_without_replay() {
        let mut log = EventLog::default();
        for cursor in 1..=5 {
            log.events
                .push_back(OperationalEvent::new(cursor, None, "test", "ok", None));
            log.next_cursor = cursor;
        }

        let page = log.page(log.next_cursor, 64);
        assert!(page.events.is_empty());
        assert_eq!(page.cursor, 5);
        assert_eq!(page.latest_cursor, 5);
        assert!(!page.cursor_lost);
    }

    #[test]
    fn reports_cursor_loss_only_after_the_last_evicted_cursor() {
        let mut log = EventLog::default();
        for cursor in 1..=(EVENT_CAPACITY as u64 + 2) {
            log.events
                .push_back(OperationalEvent::new(cursor, None, "test", "ok", None));
            if log.events.len() > EVENT_CAPACITY {
                log.events.pop_front();
            }
            log.next_cursor = cursor;
        }

        assert_eq!(log.events.front().map(|event| event.cursor), Some(3));
        assert!(log.page(1, 10).cursor_lost);
        assert!(!log.page(2, 10).cursor_lost);
        assert_eq!(log.page(2, 10).events[0].cursor, 3);
    }

    #[test]
    fn unknown_event_values_are_serialized_as_safe_typed_values() {
        let event = OperationalEvent::new(
            1,
            None,
            "Authorization: Bearer private-key-secret",
            "Cookie=session-secret",
            None,
        );
        let json = serde_json::to_string(&event).expect("event JSON");

        assert!(json.contains(r#""event":"unknown""#));
        assert!(json.contains(r#""outcome":"unknown""#));
        assert!(!json.contains("private-key-secret"));
        assert!(!json.contains("session-secret"));
    }
}
