mod clients;
mod conns;
mod count;
mod requests;
mod rtmp;

use serde::de::DeserializeOwned;
use serde_json::Value;

pub(crate) use clients::{Aggregate, ClientRow, ClientsDoc, ClientsQuery, ClientsSort};
pub(crate) use conns::{Conn, ConnsDoc, ConnsQuery};
pub(crate) use requests::{Request, Requests};
pub(crate) use rtmp::{RtmpDoc, Session, Stream};

use super::{
    model::{Filter, Notice},
    views::View,
};
use crate::{
    cli::{
        error::CliError,
        net::{Client, encode},
    },
    observability::stats_page::model::{SCHEMA_VERSION, StatsSnapshot},
};

pub(crate) const STATS_PATH: &str = "/api/v1/stats";
const REQUESTS_PATH: &str = "/api/v1/requests?limit=1000";
const RTMP_PATH: &str = "/api/v1/rtmp/stats";
const CLIENTS_PATH: &str = "/api/v1/clients";
const CONNECTIONS_PATH: &str = "/api/v1/connections";

#[derive(Debug, Default)]
pub(crate) enum Feed<T> {
    #[default]
    Off,
    Ready(T),
    Unavailable(Notice),
}

impl<T> Feed<T> {
    pub(crate) const fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(value) => Some(value),
            _ => None,
        }
    }
}

pub(crate) struct Sample {
    pub(crate) snapshot: StatsSnapshot,
    pub(crate) requests: Feed<Requests>,
    pub(crate) clients: Feed<ClientsDoc>,
    pub(crate) conns: Feed<ConnsDoc>,
    pub(crate) rtmp: Feed<RtmpDoc>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Wants {
    pub(crate) requests: bool,
    pub(crate) rtmp: bool,
    pub(crate) clients: Option<ClientsQuery>,
    pub(crate) conns: Option<ConnsQuery>,
}

const CONNECTIONS_LIMIT: usize = 1_000;

impl Wants {
    pub(crate) fn for_view(view: View, sort_key: &str, filter: &Filter, limit: usize) -> Self {
        let prefix = |field| filter.prefix(field).map(str::to_owned);
        match view {
            View::Requests => Self {
                requests: true,
                ..Self::default()
            },
            View::Rtmp => Self {
                rtmp: true,
                ..Self::default()
            },
            View::Clients => Self {
                clients: Some(ClientsQuery {
                    limit,
                    sort: ClientsSort::for_column(sort_key),
                    ip: prefix("ip"),
                }),
                ..Self::default()
            },
            View::Conns => Self {
                conns: Some(ConnsQuery {
                    limit: CONNECTIONS_LIMIT,
                    ip: prefix("ip"),
                    listener: prefix("listener"),
                }),
                ..Self::default()
            },
            View::Listeners | View::Upstreams | View::Generations => Self::default(),
        }
    }

    pub(crate) fn merged(self, other: Self) -> Self {
        Self {
            requests: self.requests || other.requests,
            rtmp: self.rtmp || other.rtmp,
            clients: self.clients.or(other.clients),
            conns: self.conns.or(other.conns),
        }
    }
}

fn read<T: DeserializeOwned>(client: &Client, path: &str, what: &str) -> Result<T, CliError> {
    let response = client.get(path, true)?;
    serde_json::from_slice(&response.body)
        .map_err(|error| CliError::Remote(format!("the {what} is not understood: {error}")))
}

pub(crate) fn snapshot(client: &Client) -> Result<StatsSnapshot, CliError> {
    let snapshot: StatsSnapshot = read(client, STATS_PATH, "stats snapshot")?;
    if snapshot.schema_version == SCHEMA_VERSION {
        Ok(snapshot)
    } else {
        Err(CliError::Remote(format!(
            "the stats snapshot has schema version {}, this client reads {SCHEMA_VERSION}",
            snapshot.schema_version
        )))
    }
}

fn mentions_disabled(text: &str) -> bool {
    text.to_ascii_lowercase().contains("disabled")
}

fn feed<T: DeserializeOwned>(client: &Client, path: &str, what: &'static str) -> Feed<T> {
    let response = match client.get(path, true) {
        Ok(response) => response,
        Err(CliError::Status(404, _)) => return Feed::Unavailable(Notice::Lacks(what)),
        Err(CliError::Status(_, text)) if mentions_disabled(&text) => {
            return Feed::Unavailable(Notice::Disabled(what));
        }
        Err(error) => return Feed::Unavailable(Notice::Failed(error.to_string())),
    };
    let document: Value = match serde_json::from_slice(&response.body) {
        Ok(document) => document,
        Err(error) => return Feed::Unavailable(Notice::Failed(error.to_string())),
    };
    let switched_off = document.get("enabled") == Some(&Value::Bool(false))
        || document.get("disabled") == Some(&Value::Bool(true));
    if switched_off {
        return Feed::Unavailable(Notice::Disabled(what));
    }
    serde_json::from_value(document).map_or_else(
        |error| Feed::Unavailable(Notice::Failed(error.to_string())),
        Feed::Ready,
    )
}

fn optional<T: DeserializeOwned>(
    client: &Client,
    wanted: bool,
    path: &str,
    what: &'static str,
) -> Feed<T> {
    if wanted {
        feed(client, path, what)
    } else {
        Feed::Off
    }
}

fn clients_path(query: &ClientsQuery) -> String {
    let mut path = format!(
        "{CLIENTS_PATH}?limit={}&sort={}",
        query.limit,
        query.sort.name()
    );
    if let Some(ip) = &query.ip {
        path = format!("{path}&ip={}", encode(ip));
    }
    path
}

fn conns_path(query: &ConnsQuery) -> String {
    let mut path = format!("{CONNECTIONS_PATH}?limit={}", query.limit);
    if let Some(ip) = &query.ip {
        path = format!("{path}&ip={}", encode(ip));
    }
    if let Some(listener) = &query.listener {
        path = format!("{path}&listener={}", encode(listener));
    }
    path
}

pub(crate) fn sample(client: &Client, wants: &Wants) -> Result<Sample, CliError> {
    Ok(Sample {
        snapshot: snapshot(client)?,
        requests: optional(
            client,
            wants.requests,
            REQUESTS_PATH,
            "an in-flight request list",
        ),
        rtmp: optional(client, wants.rtmp, RTMP_PATH, "RTMP statistics"),
        clients: wants.clients.as_ref().map_or(Feed::Off, |query| {
            feed(client, &clients_path(query), "per-client tracking")
        }),
        conns: wants.conns.as_ref().map_or(Feed::Off, |query| {
            feed(client, &conns_path(query), "per-connection tracking")
        }),
    })
}
