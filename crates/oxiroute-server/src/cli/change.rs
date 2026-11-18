use clap::Args;
use serde_json::Value;

use super::{
    error::CliError,
    net::Client,
    render::{Document, Field},
    view,
};

#[derive(Args, Clone, Copy)]
pub(crate) struct Persist {
    /// Keep the change in the overlay file beside the configuration, so it survives a restart.
    #[arg(long)]
    pub(crate) persist: bool,
}

impl Persist {
    pub(crate) fn body(self, mut fields: Value) -> Value {
        if self.persist {
            fields["persist"] = true.into();
        }
        fields
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Scope {
    Server,
    Pool,
    Listener,
}

#[derive(Clone, Copy)]
pub(crate) enum Setting {
    State,
    Weight,
    MaxConnections,
    Health,
    Checks,
}

pub(crate) enum Desired {
    Value(String),
    Configured,
}

pub(crate) struct Plan {
    scope: Scope,
    setting: Setting,
    desired: Desired,
    targets: Vec<String>,
}

pub(crate) struct Observation {
    current: Option<String>,
    configured: Option<String>,
}

impl Scope {
    const fn path(self) -> &'static str {
        match self {
            Self::Server => "/api/v1/servers",
            Self::Pool => "/api/v1/pools",
            Self::Listener => "/api/v1/listeners",
        }
    }

    const fn collection(self) -> &'static str {
        match self {
            Self::Server => "servers",
            Self::Pool => "pools",
            Self::Listener => "listeners",
        }
    }

    const fn noun(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Pool => "pool",
            Self::Listener => "listener",
        }
    }

    fn find<'a>(self, entries: &'a Value, target: &str) -> Option<&'a Value> {
        let entries = entries.get(self.collection())?.as_array()?;
        entries
            .iter()
            .find(|entry| self.key(entry).as_deref() == Some(target))
    }

    fn key(self, entry: &Value) -> Option<String> {
        let name = |value: &Value| value.get("name")?.as_str().map(str::to_owned);
        match self {
            Self::Server => {
                let pool = entry.get("pool")?.as_str()?;
                Some(format!("{pool}/{}", name(entry.get("server")?)?))
            }
            Self::Pool | Self::Listener => name(entry),
        }
    }
}

impl Setting {
    const fn name(self) -> &'static str {
        match self {
            Self::State => "state",
            Self::Weight => "weight",
            Self::MaxConnections => "max_connections",
            Self::Health => "health",
            Self::Checks => "checks",
        }
    }

    fn observe(self, scope: Scope, entry: &Value) -> Observation {
        let text =
            |value: &Value, key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        let limit = |value: &Value, key: &str| {
            Some(
                value
                    .get(key)
                    .and_then(Value::as_u64)
                    .map_or_else(|| "none".to_owned(), |limit| limit.to_string()),
            )
        };
        let server = entry.get("server").unwrap_or(entry);
        let current = match (scope, self) {
            (Scope::Pool, _) => view::pool_state(entry),
            (_, Self::State) => text(server, "administrativeState"),
            (_, Self::Weight) => server
                .get("weight")
                .and_then(Value::as_u64)
                .map(|weight| weight.to_string()),
            (_, Self::MaxConnections) => limit(server, "maxConnections"),
            (_, Self::Health) => text(server, "healthOverride"),
            (_, Self::Checks) => server
                .get("checksEnabled")
                .and_then(Value::as_bool)
                .map(|enabled| if enabled { "enabled" } else { "disabled" }.to_owned()),
        };
        let configured = matches!(self, Self::MaxConnections)
            .then(|| limit(server, "configuredMaxConnections"))
            .flatten();
        Observation {
            current,
            configured,
        }
    }
}

impl Plan {
    pub(crate) const fn new(
        scope: Scope,
        setting: Setting,
        desired: Desired,
        targets: Vec<String>,
    ) -> Self {
        Self {
            scope,
            setting,
            desired,
            targets,
        }
    }

    pub(crate) fn observe(&self, client: &Client) -> Result<Vec<Option<Observation>>, CliError> {
        let entries = client.get(self.scope.path(), true)?.json()?;
        Ok(self
            .targets
            .iter()
            .map(|target| {
                let entry = self.scope.find(&entries, target)?;
                Some(self.setting.observe(self.scope, entry))
            })
            .collect())
    }

    pub(crate) fn summary(&self, observed: &[Option<Observation>], persistence: &str) -> Document {
        let rows = self
            .targets
            .iter()
            .zip(observed)
            .map(|(target, observed)| {
                let (value, change) = self.outcome(observed.as_ref());
                let mut row = vec![
                    Field::text("target", Some(target)),
                    Field::text("setting", Some(self.setting.name())),
                    Field::text("value", Some(&value)),
                    Field::text("change", Some(change)),
                ];
                if persistence != "runtime" {
                    row.push(Field::text("persistence", Some(persistence)));
                }
                row
            })
            .collect();
        Document::rows(self.scope.noun(), rows)
    }

    fn outcome(&self, observation: Option<&Observation>) -> (String, &'static str) {
        let current = observation.and_then(|observation| observation.current.as_deref());
        let value = match (&self.desired, observation) {
            (Desired::Value(value), _) => Some(value.clone()),
            (Desired::Configured, observation) => {
                observation.and_then(|observation| observation.configured.clone())
            }
        };
        match (value, current) {
            (Some(value), Some(current)) if value == current => (value, "unchanged"),
            (Some(value), Some(_)) => (value, "changed"),
            (Some(value), None) => (value, "applied"),
            (None, _) => ("default".to_owned(), "applied"),
        }
    }
}

#[cfg(test)]
mod tests;
