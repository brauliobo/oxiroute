use pingora::protocols::http::ServerSession;
use serde_json::{Value, json};

use super::{REDACTION_POLICY, comparison, config_value};
use crate::{
    config::{
        coordinator::{AuthoredRevision, ConfigValidationOutcome},
        diff::unified,
        history::{ConfigHistory, HistoryEntry, HistoryError, HistoryLimits},
    },
    rtmp::api::{
        ApiResponse,
        config::{ConfigApiState, config_revision_header},
        response::system_time_ms,
    },
};

const HISTORY_PATH: &str = "/api/v1/config/history";
const CURRENT: &str = "current";

#[derive(Clone, Copy)]
pub(in crate::rtmp::api) enum HistoryRoute {
    List,
    Diff,
    Entry,
    Rollback,
}

impl HistoryRoute {
    pub(in crate::rtmp::api) const fn method(self) -> &'static str {
        match self {
            Self::Rollback => "POST",
            Self::List | Self::Diff | Self::Entry => "GET",
        }
    }
}

pub(in crate::rtmp::api::config) fn history_route(path: &str) -> Option<HistoryRoute> {
    let rest = path.strip_prefix(HISTORY_PATH)?;
    match rest.strip_prefix('/') {
        None if rest.is_empty() => Some(HistoryRoute::List),
        Some("diff") => Some(HistoryRoute::Diff),
        Some(rest) => match rest.split_once('/') {
            None if !rest.is_empty() => Some(HistoryRoute::Entry),
            Some((id, "rollback")) if !id.is_empty() => Some(HistoryRoute::Rollback),
            _ => None,
        },
        None => None,
    }
}

fn history_id(path: &str) -> &str {
    path.strip_prefix(HISTORY_PATH)
        .and_then(|rest| rest.strip_prefix('/'))
        .and_then(|rest| rest.split('/').next())
        .unwrap_or_default()
}

fn query_value<'a>(query: Option<&'a str>, name: &str) -> Option<&'a str> {
    query
        .into_iter()
        .flat_map(|query| query.split('&'))
        .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
}

fn entry_json(entry: &HistoryEntry, current_id: &str) -> Value {
    json!({
        "id": entry.id,
        "at": entry.at_unix_ms,
        "bytes": entry.bytes,
        "current": entry.id == current_id,
    })
}

fn failed(error: &HistoryError) -> ApiResponse {
    match error {
        HistoryError::NotFound => ApiResponse::error(
            404,
            "history_entry_not_found",
            "no configuration history entry has this id",
        ),
        HistoryError::Ambiguous => ApiResponse::error(
            400,
            "history_id_ambiguous",
            "the id matches several history entries; give more of it",
        ),
        HistoryError::Invalid => ApiResponse::error(
            400,
            "history_id_invalid",
            "a history id is at least 6 lowercase hexadecimal digits",
        ),
        HistoryError::Io(_) => ApiResponse::error(
            500,
            "history_unavailable",
            "the configuration history could not be read",
        ),
    }
}

impl ConfigApiState {
    pub(in crate::rtmp::api::config) fn handle_history(
        &self,
        route: HistoryRoute,
        method: &str,
        session: &mut ServerSession,
    ) -> ApiResponse {
        if method != route.method() {
            return ApiResponse::method_not_allowed(route.method());
        }
        let uri = session.req_header().uri.clone();
        match route {
            HistoryRoute::List => self.history_list(),
            HistoryRoute::Entry => self.history_entry(history_id(uri.path())),
            HistoryRoute::Diff => self.history_diff(
                query_value(uri.query(), "from"),
                query_value(uri.query(), "to"),
            ),
            HistoryRoute::Rollback => match config_revision_header(session) {
                Ok(expected) => self.history_rollback(&expected, history_id(uri.path())),
                Err(response) => response,
            },
        }
    }

    fn history_list(&self) -> ApiResponse {
        let current = match self.coordinator.read_source() {
            Ok(current) => current,
            Err(rejection) => return self.source_unavailable(&rejection),
        };
        let history = self.coordinator.history();
        let entries = match history.list() {
            Ok(entries) => entries,
            Err(error) => return failed(&HistoryError::Io(error)),
        };
        let current_id = ConfigHistory::id_of(current.text.as_bytes());
        let limits = self.coordinator.load_shared().map_or_else(
            |_| HistoryLimits::default(),
            |document| HistoryLimits::of(&document.validated_config),
        );
        ApiResponse::json(
            200,
            &json!({
                "schemaVersion": 1,
                "diskRevision": current.revision,
                "activeRevision": self.active_revision(),
                "format": current.format,
                "current": { "id": current_id, "bytes": current.text.len() },
                "limits": { "maxEntries": limits.max_entries, "maxTotalBytes": limits.max_total_bytes },
                "entries": entries.iter().map(|entry| entry_json(entry, &current_id)).collect::<Vec<_>>(),
            }),
        )
    }

    fn history_text(&self, id: &str) -> Result<(Option<HistoryEntry>, String), ApiResponse> {
        if id == CURRENT {
            let current = self
                .coordinator
                .read_source()
                .map_err(|rejection| self.source_unavailable(&rejection))?;
            return Ok((None, current.text));
        }
        let (entry, bytes) = self
            .coordinator
            .history()
            .read(id)
            .map_err(|error| failed(&error))?;
        let text = String::from_utf8(bytes).map_err(|_| {
            ApiResponse::error(
                500,
                "history_entry_unreadable",
                "the history entry is not UTF-8 text",
            )
        })?;
        Ok((Some(entry), text))
    }

    fn history_entry(&self, id: &str) -> ApiResponse {
        let (entry, text) = match self.history_text(id) {
            Ok(found) => found,
            Err(response) => return response,
        };
        let format = self.coordinator.format();
        let authority = self.authority();
        let secrets = match self.coordinator.prepare_source(&text, format) {
            ConfigValidationOutcome::Valid(candidate) => {
                Some(authority.secrets_with(candidate.validated_config().as_draft()))
            }
            ConfigValidationOutcome::Invalid(_) => None,
        };
        let withheld = secrets.as_ref().is_none_or(|secrets| !secrets.can_redact());
        let redacted = secrets.as_ref().map(|secrets| secrets.redact(&text));
        let current_id = self
            .coordinator
            .read_source()
            .map(|current| ConfigHistory::id_of(current.text.as_bytes()))
            .unwrap_or_default();
        ApiResponse::json(
            200,
            &json!({
                "schemaVersion": 1,
                "id": entry.as_ref().map_or_else(|| ConfigHistory::id_of(text.as_bytes()), |entry| entry.id.clone()),
                "at": entry.as_ref().map(|entry| entry.at_unix_ms),
                "bytes": text.len(),
                "revision": AuthoredRevision::from_bytes(text.as_bytes()),
                "format": format,
                "current": ConfigHistory::id_of(text.as_bytes()) == current_id,
                "text": redacted.as_ref().filter(|_| !withheld).map(|redacted| redacted.text.as_str()),
                "redaction": {
                    "policy": REDACTION_POLICY,
                    "redacted": redacted.as_ref().map_or(0, |redacted| redacted.replaced),
                    "withheld": withheld,
                },
            }),
        )
    }

    fn history_diff(&self, from: Option<&str>, to: Option<&str>) -> ApiResponse {
        let (Some(from), Some(to)) = (from, to) else {
            return ApiResponse::error(
                400,
                "history_diff_needs_two_ids",
                "from and to name two history ids, or `current`",
            );
        };
        let (older, newer) = match (self.history_text(from), self.history_text(to)) {
            (Ok((_, older)), Ok((_, newer))) => (older, newer),
            (Err(response), _) | (_, Err(response)) => return response,
        };
        let format = self.coordinator.format();
        let resolved =
            [&older, &newer].map(|text| match self.coordinator.prepare_source(text, format) {
                ConfigValidationOutcome::Valid(candidate) => Some(candidate),
                ConfigValidationOutcome::Invalid(_) => None,
            });
        let [Some(older_candidate), Some(newer_candidate)] = &resolved else {
            return ApiResponse::json(
                200,
                &json!({
                    "schemaVersion": 1,
                    "identical": older == newer,
                    "diff": null,
                    "textDiff": null,
                    "unresolved": true,
                }),
            );
        };
        let authority = self.authority();
        let secrets = authority.secrets_with_all(&[
            older_candidate.validated_config().as_draft(),
            newer_candidate.validated_config().as_draft(),
        ]);
        let text_diff = secrets.can_redact().then(|| {
            unified(
                &secrets.redact(&older).text,
                &secrets.redact(&newer).text,
                from,
                to,
            )
        });
        ApiResponse::json(
            200,
            &json!({
                "schemaVersion": 1,
                "identical": older == newer,
                "diff": comparison(
                    None,
                    &config_value(older_candidate.validated_config()),
                    &config_value(newer_candidate.validated_config()),
                ),
                "textDiff": text_diff,
                "unresolved": false,
            }),
        )
    }

    fn history_rollback(&self, expected: &AuthoredRevision, id: &str) -> ApiResponse {
        if let Err(response) = system_time_ms() {
            return response;
        }
        let text = match self.history_text(id) {
            Ok((Some(_), text)) => text,
            Ok((None, _)) => {
                return ApiResponse::error(
                    400,
                    "history_id_invalid",
                    "the current configuration cannot be rolled back to",
                );
            }
            Err(response) => return response,
        };
        let format = self.coordinator.format();
        self.persist(expected, |mutation| {
            let now_unix_ms = system_time_ms()?;
            self.preflight(
                self.coordinator.prepare_source(&text, format),
                now_unix_ms,
                mutation,
            )
        })
    }
}
