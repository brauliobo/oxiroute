//! Changes of a configuration that only a process restart applies.
//!
//! One computation decides them for every surface: the watcher, `SIGHUP`, `generation reload`,
//! `generation status`, `status`, `config validate` and `config apply` all report the entries of
//! [`startup_only_changes`], so one candidate carries the same code, path and summary everywhere.

use std::{
    net::SocketAddr,
    time::{SystemTime, UNIX_EPOCH},
};

use oxiroute_config::{ConfigDraft, ListenerBind, Registry, ValidatedConfig};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ListenerReservations, PendingListenerChange, RuntimeMode};

/// One setting of a candidate configuration that a running process cannot take over.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct StartupOnlyChange {
    pub code: String,
    pub path: String,
    pub summary: String,
}

/// Whether the file on disk holds settings that wait for a restart, and which.
#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestartRequired {
    pub required: bool,
    /// The revision of the file on disk that carries the waiting settings.
    pub candidate_revision: Option<String>,
    /// Unix milliseconds at which the restart became required.
    pub since: Option<u64>,
    pub reasons: Vec<StartupOnlyChange>,
}

impl StartupOnlyChange {
    /// The candidate needs the direct runtime, which a supervising master cannot run.
    #[must_use]
    pub fn execution_mode(reason: &str) -> Self {
        Self {
            code: "execution_mode".into(),
            path: "/config".into(),
            summary: format!(
                "the configuration cannot run under the supervising master ({reason}) and is not \
                 applied: it runs after a restart in the direct runtime"
            ),
        }
    }
}

impl RestartRequired {
    /// Says in the log, once per change of the report, what waits for a restart.
    pub fn log(&self) {
        let revision = self.candidate_revision.as_deref().unwrap_or("unknown");
        if !self.required {
            log::info!(target: "oxiroute::config", "no configuration change waits for a restart");
        }
        for reason in &self.reasons {
            log::warn!(
                target: "oxiroute::config",
                "configuration revision {revision} is applied except for a setting that only a \
                 restart applies: {}: {}",
                reason.path,
                reason.summary
            );
        }
    }

    /// The report after `reasons` were found for `candidate_revision`; `since` carries over while a
    /// restart stays required.
    #[must_use]
    pub fn update(&self, candidate_revision: &str, reasons: Vec<StartupOnlyChange>) -> Self {
        if reasons.is_empty() {
            return Self::default();
        }
        Self {
            required: true,
            candidate_revision: Some(candidate_revision.to_owned()),
            since: self
                .since
                .filter(|_| self.required)
                .or_else(|| Some(unix_millis())),
            reasons,
        }
    }
}

/// A setting that the running process cannot change: the candidate asks for `wanted` while the
/// process keeps `running` until it restarts.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "setting", rename_all = "snake_case")]
pub enum StartupOnly {
    ListenerIpv6Only {
        listener: String,
        address: SocketAddr,
        running: bool,
        wanted: bool,
    },
    RegistrySettings {
        running: Registry,
    },
}

impl StartupOnly {
    fn from_pending(change: PendingListenerChange) -> Option<Self> {
        match (change.code, change.bind) {
            ("listener_ipv6_only", ListenerBind::Socket { address, ipv6_only }) => {
                Some(Self::ListenerIpv6Only {
                    listener: change.listener,
                    address,
                    running: !ipv6_only,
                    wanted: ipv6_only,
                })
            }
            _ => None,
        }
    }

    /// Puts the value the process keeps back into `draft`, so that the rest of the candidate can
    /// be applied.
    pub fn keep_running(&self, draft: &mut ConfigDraft) {
        match self {
            Self::ListenerIpv6Only {
                listener, running, ..
            } => {
                for bind in listener_binds(draft, listener) {
                    if let ListenerBind::Socket { ipv6_only, .. } = bind {
                        *ipv6_only = *running;
                    }
                }
            }
            Self::RegistrySettings { running } => draft.registry = Some(running.clone()),
        }
    }

    #[must_use]
    pub fn change(&self) -> StartupOnlyChange {
        match self {
            Self::ListenerIpv6Only {
                listener,
                address,
                running,
                wanted,
            } => StartupOnlyChange {
                code: "listener_ipv6_only".into(),
                path: format!("/config/listeners/{listener}/bind/ipv6_only"),
                summary: format!(
                    "listener `{listener}` keeps ipv6_only {running} on {address}: a bound socket \
                     cannot change it, and the configured {wanted} takes effect after a restart",
                ),
            },
            Self::RegistrySettings { .. } => StartupOnlyChange {
                code: "registry_settings".into(),
                path: "/config/registry".into(),
                summary:
                    "the direct runtime opens the registry once: its state_dir, tokens_file and \
                          endpoints change only after a restart"
                        .into(),
            },
        }
    }
}

fn listener_binds<'a>(
    draft: &'a mut ConfigDraft,
    name: &'a str,
) -> impl Iterator<Item = &'a mut ListenerBind> {
    draft
        .listeners
        .iter_mut()
        .filter(move |listener| listener.name == name)
        .map(|listener| &mut listener.bind)
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// The settings of `wanted` that a process running `running` in `mode` keeps as they are.
///
/// An entry here is a setting no reload of that runtime applies. A change that becomes live leaves
/// this function, and every surface stops reporting it.
#[must_use]
pub fn startup_only_changes(
    mode: RuntimeMode,
    running: &ValidatedConfig,
    wanted: &ValidatedConfig,
) -> Vec<StartupOnly> {
    let mut changes = ListenerReservations::pending_startup_listener_changes(running, wanted)
        .into_iter()
        .filter_map(StartupOnly::from_pending)
        .collect::<Vec<_>>();
    let registry = &running.as_draft().registry;
    if mode == RuntimeMode::Direct && registry.is_some() && *registry != wanted.as_draft().registry
    {
        changes.extend(
            registry
                .clone()
                .map(|running| StartupOnly::RegistrySettings { running }),
        );
    }
    changes
}

/// The report entries of `changes`.
#[must_use]
pub fn startup_only_reasons(changes: &[StartupOnly]) -> Vec<StartupOnlyChange> {
    changes.iter().map(StartupOnly::change).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(bind: &str, registry: &str) -> ValidatedConfig {
        oxiroute_config_source::load_lua(&format!(
            r#"return {{
              version = 1,
              {registry}
              listeners = {{ {{ name = "live", bind = {bind}, protocol = "rtmp", service = "live" }} }},
              rtmp_services = {{ {{ name = "live",
                applications = {{ {{ name = "live", live = true, idle_streams = true, recorders = {{}} }} }} }} }},
            }}"#
        ))
        .expect("valid test configuration")
    }

    fn unix(mode: u16) -> String {
        format!(r#"{{ type = "unix", path = "/run/oxiroute/live.sock", mode = {mode} }}"#)
    }

    fn tcp(ipv6_only: bool) -> String {
        format!(r#"{{ type = "socket", address = "[::]:1935", ipv6_only = {ipv6_only} }}"#)
    }

    #[test]
    fn a_unix_socket_that_keeps_its_path_takes_its_mode_in_place() {
        let running = config(&unix(0o600), "");
        let wanted = config(&unix(0o660), "");

        assert!(startup_only_changes(RuntimeMode::Supervised, &running, &wanted).is_empty());
        assert!(startup_only_changes(RuntimeMode::Direct, &running, &wanted).is_empty());
    }

    #[test]
    fn a_socket_that_keeps_its_address_keeps_ipv6_only() {
        let changes = startup_only_changes(
            RuntimeMode::Direct,
            &config(&tcp(false), ""),
            &config(&tcp(true), ""),
        );

        assert_eq!(
            startup_only_reasons(&changes),
            vec![StartupOnlyChange {
                code: "listener_ipv6_only".into(),
                path: "/config/listeners/live/bind/ipv6_only".into(),
                summary: "listener `live` keeps ipv6_only false on [::]:1935: a bound socket \
                          cannot change it, and the configured true takes effect after a restart"
                    .into(),
            }]
        );
    }

    #[test]
    fn only_the_direct_runtime_keeps_its_registry() {
        let registry = |endpoint: &str| {
            format!(
                r#"registry = {{ state_dir = "/var/lib/oxiroute/registry", endpoints = {{ {endpoint} }} }},"#
            )
        };
        let running = config(&unix(0o600), &registry(""));
        let wanted = config(
            &unix(0o600),
            &registry(r#"{ type = "unix", path = "/run/oxiroute/registry.sock", role = "read" }"#),
        );

        let direct = startup_only_changes(RuntimeMode::Direct, &running, &wanted);

        assert_eq!(startup_only_reasons(&direct)[0].code, "registry_settings");
        assert!(startup_only_changes(RuntimeMode::Supervised, &running, &wanted).is_empty());
        let unconfigured = config(&unix(0o600), "");
        assert!(startup_only_changes(RuntimeMode::Direct, &unconfigured, &wanted).is_empty());
    }

    #[test]
    fn the_restart_report_keeps_its_start_while_it_stays_required() {
        let reason = StartupOnlyChange {
            code: "listener_ipv6_only".into(),
            path: "/config/listeners/live/bind/ipv6_only".into(),
            summary: "kept".into(),
        };
        let first = RestartRequired::default().update("a", vec![reason.clone()]);
        let second = first.update("b", vec![reason]);

        assert!(first.required);
        assert_eq!(second.since, first.since);
        assert_eq!(second.candidate_revision.as_deref(), Some("b"));
        assert_eq!(second.update("b", Vec::new()), RestartRequired::default());
    }

    #[test]
    fn keeping_the_running_settings_leaves_nothing_to_keep() {
        let running = config(&tcp(false), "");
        let wanted = config(&tcp(true), "");
        let kept = startup_only_changes(RuntimeMode::Direct, &running, &wanted);
        let mut draft = wanted.to_draft();

        for setting in &kept {
            setting.keep_running(&mut draft);
        }

        let applied = draft.validate().expect("valid with the kept settings");
        assert_eq!(applied, running);
        assert!(startup_only_changes(RuntimeMode::Direct, &running, &applied).is_empty());
    }
}
