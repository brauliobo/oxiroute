use schemars::JsonSchema;
use serde::Serialize;

/// Where a mutation lives: only in this process (`runtime`: it follows its target across generation
/// changes and is lost at a restart), in the dynamic-server registry (`registry`: it is stored with
/// the registration and survives a restart), or both for a batch that touches static and registered
/// servers (`mixed`).
#[derive(Clone, Copy, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Persistence {
    Runtime,
    Registry,
    Mixed,
}

impl Persistence {
    pub(crate) fn all(scopes: impl IntoIterator<Item = Self>) -> Self {
        scopes
            .into_iter()
            .reduce(|left, right| if left == right { left } else { Self::Mixed })
            .unwrap_or(Self::Runtime)
    }
}

#[cfg(test)]
mod tests {
    use super::Persistence::{self, Mixed, Registry, Runtime};

    #[test]
    fn a_batch_is_mixed_only_when_its_targets_differ() {
        assert_eq!(Persistence::all([]), Runtime);
        assert_eq!(Persistence::all([Runtime, Runtime]), Runtime);
        assert_eq!(Persistence::all([Registry, Registry]), Registry);
        assert_eq!(Persistence::all([Registry, Runtime, Registry]), Mixed);
        assert_eq!(Persistence::all([Mixed, Registry]), Mixed);
    }
}
