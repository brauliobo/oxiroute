use oxiroute_config::{ListenerBind, Protocol, ValidatedConfig};

use crate::MetricsError;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ListenerId {
    Traffic(String),
    Management,
    Stats(usize),
    StatsPage(usize),
    Legacy(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ListenerPlane {
    Control,
    Data,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ListenerDescriptorRole {
    Traffic(String),
    Management,
    Stats(usize),
    StatsPage(usize),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ListenerDescriptorKind {
    Tcp,
    Unix,
    Datagram,
    Quic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ListenerMetricPolicy {
    Public,
    InternalOnly,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ListenerRestartReason {
    /// A Unix socket keeps its path and changes its mode, which a running socket cannot do.
    UnixModeChange,
}

/// What a reload does to the listener sockets, as operator-facing labels.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ListenerChangeSummary {
    /// Listeners bound before traffic switches.
    pub(crate) added: Vec<String>,
    /// Listeners that stop accepting and close once their connections finish.
    pub(crate) removed: Vec<String>,
    /// Listeners whose sockets carry over without interruption.
    pub(crate) kept: Vec<String>,
}

impl ListenerChangeSummary {
    pub(crate) fn changes_listeners(&self) -> bool {
        !self.added.is_empty() || !self.removed.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ListenerEntry {
    pub(crate) id: ListenerId,
    pub(crate) name: String,
    pub(crate) protocol: Protocol,
    pub(crate) bind: ListenerBind,
    pub(crate) plane: ListenerPlane,
    pub(crate) descriptor_role: ListenerDescriptorRole,
    pub(crate) descriptor_kind: ListenerDescriptorKind,
    pub(crate) metric_policy: ListenerMetricPolicy,
    pub(crate) max_connections: Option<u64>,
}

impl ListenerEntry {
    pub(crate) const fn descriptor_protocol(&self) -> Option<Protocol> {
        match self.plane {
            ListenerPlane::Control => None,
            ListenerPlane::Data => Some(self.protocol),
        }
    }

    pub(crate) const fn protocol_name(&self) -> &'static str {
        match self.protocol {
            Protocol::Http => "http",
            Protocol::Rtmp => "rtmp",
            Protocol::Tcp => "tcp",
            Protocol::Udp => "udp",
            Protocol::ForwardHttp1 => "forward_http1",
            Protocol::ForwardHttp2 => "forward_http2",
            Protocol::ForwardHttp3 => "forward_http3",
            Protocol::Http3 => "http3",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ListenerInventory {
    entries: Box<[ListenerEntry]>,
}

impl ListenerInventory {
    pub(crate) fn compile(config: &ValidatedConfig) -> Self {
        let config = config.as_draft();
        let count = config.listeners.len()
            + usize::from(config.management.is_some())
            + config
                .stats
                .as_ref()
                .map_or(0, |stats| stats.binds.len() + stats.pages.len());
        let mut entries = Vec::with_capacity(count);

        entries.extend(config.listeners.iter().map(|listener| ListenerEntry {
            id: ListenerId::Traffic(listener.name.clone()),
            name: listener.name.clone(),
            protocol: listener.protocol,
            bind: listener.bind.clone(),
            plane: ListenerPlane::Data,
            descriptor_role: ListenerDescriptorRole::Traffic(listener.name.clone()),
            descriptor_kind: descriptor_kind(listener.protocol, &listener.bind),
            metric_policy: ListenerMetricPolicy::Public,
            max_connections: listener.max_connections,
        }));

        if let Some(management) = &config.management {
            entries.push(ListenerEntry {
                id: ListenerId::Management,
                name: "@management".into(),
                protocol: Protocol::Http,
                bind: ListenerBind::Socket {
                    address: management.bind,
                },
                plane: ListenerPlane::Control,
                descriptor_role: ListenerDescriptorRole::Management,
                descriptor_kind: ListenerDescriptorKind::Tcp,
                metric_policy: ListenerMetricPolicy::InternalOnly,
                max_connections: None,
            });
        }
        if let Some(stats) = &config.stats {
            entries.extend(
                stats
                    .binds
                    .iter()
                    .enumerate()
                    .map(|(index, address)| ListenerEntry {
                        id: ListenerId::Stats(index),
                        name: format!("@stats-{index}"),
                        protocol: Protocol::Http,
                        bind: ListenerBind::Socket { address: *address },
                        plane: ListenerPlane::Control,
                        descriptor_role: ListenerDescriptorRole::Stats(index),
                        descriptor_kind: ListenerDescriptorKind::Tcp,
                        metric_policy: ListenerMetricPolicy::InternalOnly,
                        max_connections: None,
                    }),
            );
            entries.extend(
                stats
                    .pages
                    .iter()
                    .enumerate()
                    .map(|(index, page)| ListenerEntry {
                        id: ListenerId::StatsPage(index),
                        name: format!("@stats-page-{index}"),
                        protocol: Protocol::Http,
                        bind: ListenerBind::Socket { address: page.bind },
                        plane: ListenerPlane::Control,
                        descriptor_role: ListenerDescriptorRole::StatsPage(index),
                        descriptor_kind: ListenerDescriptorKind::Tcp,
                        metric_policy: ListenerMetricPolicy::Public,
                        max_connections: page.max_connections,
                    }),
            );
        }

        Self {
            entries: entries.into_boxed_slice(),
        }
    }

    pub(crate) fn entries(&self) -> &[ListenerEntry] {
        &self.entries
    }

    pub(crate) fn validate_public_display_names(&self) -> Result<(), MetricsError> {
        let mut names = std::collections::HashSet::new();
        for entry in &self.entries {
            if entry.metric_policy == ListenerMetricPolicy::Public && !names.insert(&entry.name) {
                return Err(MetricsError::DuplicateListener(entry.name.clone()));
            }
        }
        Ok(())
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether both inventories need exactly the same descriptors in the same order.
    ///
    /// A supervised worker only adopts sockets, so it cannot apply a listener-set change on its
    /// own: the supervising master binds the new sockets and closes the removed ones.
    pub(crate) fn same_descriptor_topology(&self, candidate: &Self) -> bool {
        self.entries.len() == candidate.entries.len()
            && self
                .entries
                .iter()
                .zip(candidate.entries.iter())
                .all(|(active, candidate)| {
                    active.id == candidate.id
                        && active.descriptor_role == candidate.descriptor_role
                        && active.descriptor_kind == candidate.descriptor_kind
                        && active.descriptor_protocol() == candidate.descriptor_protocol()
                        && active.bind == candidate.bind
                })
    }

    /// Returns why `candidate` cannot replace this inventory by a reload.
    ///
    /// Listeners that are added, removed, renamed, reordered or given another protocol or TLS
    /// profile never need a restart: a reload binds the new sockets, carries over the sockets whose
    /// bind address is unchanged and closes the removed ones after draining. Only a Unix socket
    /// that keeps its path and changes its mode is refused, because the new socket would need the
    /// path the running one owns.
    pub(crate) fn restart_reason(&self, candidate: &Self) -> Option<ListenerRestartReason> {
        candidate
            .entries
            .iter()
            .any(|candidate| {
                let ListenerBind::Unix {
                    path: candidate_path,
                    mode: candidate_mode,
                } = &candidate.bind
                else {
                    return false;
                };
                self.entries.iter().any(|active| {
                    matches!(
                        &active.bind,
                        ListenerBind::Unix { path, mode }
                            if path == candidate_path && mode != candidate_mode
                    )
                })
            })
            .then_some(ListenerRestartReason::UnixModeChange)
    }

    pub(crate) fn restart_required(&self, candidate: &Self) -> bool {
        self.restart_reason(candidate).is_some()
    }

    pub(crate) fn complete_listener_count(&self) -> usize {
        self.entries.len()
    }
}

fn descriptor_kind(protocol: Protocol, bind: &ListenerBind) -> ListenerDescriptorKind {
    match bind {
        ListenerBind::Socket { .. } => ListenerDescriptorKind::Tcp,
        ListenerBind::Unix { .. } => ListenerDescriptorKind::Unix,
        ListenerBind::Udp { .. } => match protocol {
            Protocol::Udp => ListenerDescriptorKind::Datagram,
            Protocol::ForwardHttp3 | Protocol::Http3 => ListenerDescriptorKind::Quic,
            Protocol::Http
            | Protocol::Rtmp
            | Protocol::Tcp
            | Protocol::ForwardHttp1
            | Protocol::ForwardHttp2 => {
                unreachable!("validated configuration paired a stream protocol with a UDP bind")
            }
        },
    }
}

#[cfg(test)]
mod tests;
