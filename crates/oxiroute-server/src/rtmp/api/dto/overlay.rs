use schemars::JsonSchema;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OverlayStatus {
    Applied,
    Diverged,
    Orphan,
}

#[derive(Clone, Copy, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OverlayState {
    Empty,
    Applied,
    Diverged,
    Unreadable,
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct OverlaySetting {
    pub(crate) target: String,
    pub(crate) setting: &'static str,
    pub(crate) value: String,
    pub(crate) status: OverlayStatus,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OverlayResponse {
    pub(crate) path: String,
    pub(crate) revision: Option<String>,
    pub(crate) state: OverlayState,
    pub(crate) settings: Vec<OverlaySetting>,
}

impl OverlayResponse {
    pub(crate) fn unreadable(path: String) -> Self {
        Self {
            path,
            revision: None,
            state: OverlayState::Unreadable,
            settings: Vec::new(),
        }
    }

    pub(crate) fn of(
        path: String,
        revision: Option<String>,
        settings: Vec<OverlaySetting>,
    ) -> Self {
        let state = if settings.is_empty() {
            OverlayState::Empty
        } else if settings
            .iter()
            .all(|setting| setting.status == OverlayStatus::Applied)
        {
            OverlayState::Applied
        } else {
            OverlayState::Diverged
        };
        Self {
            path,
            revision,
            state,
            settings,
        }
    }
}
