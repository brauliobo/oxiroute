mod config;
mod contexts;
mod events;
mod generation;
mod generic;
mod listeners;
mod monitoring;
mod outcome;
mod pools;
mod read;
mod requests;
mod rtmp;
mod servers;
mod status;
mod tls;
mod topology;

use serde_json::Value;

use super::render::Document;

pub(crate) fn rejection_summary(generation: &Value) -> Option<String> {
    generation::rejection(generation)
}

pub(crate) fn pool_state(pool: &Value) -> Option<String> {
    pools::administrative_state(pool)
}

#[derive(Clone, Copy)]
pub(crate) enum View {
    Generic,
    Status,
    Monitoring,
    Topology,
    Config,
    Contexts,
    Pools,
    Pool,
    Servers,
    Server,
    Listeners,
    Listener,
    Generation,
    Tls,
    Events,
    Requests,
    Request,
    RtmpStreams,
    RtmpStream,
    Evacuation,
    DnsRefresh,
}

impl View {
    pub(crate) fn document(self, value: &Value) -> Document {
        match self {
            Self::Generic => generic::show(value),
            Self::Status => status::show(value),
            Self::Monitoring => monitoring::show(value),
            Self::Topology => topology::show(value),
            Self::Config => config::show(value),
            Self::Contexts => contexts::list(value),
            Self::Pools => pools::list(value),
            Self::Pool => pools::detail(value),
            Self::Servers => servers::list(value),
            Self::Server => servers::detail(value),
            Self::Listeners => listeners::list(value),
            Self::Listener => listeners::detail(value),
            Self::Generation => generation::status(value),
            Self::Tls => tls::list(value),
            Self::Events => events::list(value),
            Self::Requests => requests::list(value),
            Self::Request => requests::detail(value),
            Self::RtmpStreams => rtmp::list(value),
            Self::RtmpStream => rtmp::detail(value),
            Self::Evacuation => outcome::evacuation(value),
            Self::DnsRefresh => outcome::dns_refresh(value),
        }
    }
}

#[cfg(test)]
mod tests;
