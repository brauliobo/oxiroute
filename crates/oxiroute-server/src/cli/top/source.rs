use serde::de::DeserializeOwned;

use super::rows::Requests;
use crate::{
    cli::{CliError, Client},
    observability::stats_page::model::{SCHEMA_VERSION, StatsSnapshot},
};

pub(super) const STATS_PATH: &str = "/api/v1/stats";
const REQUESTS_PATH: &str = "/api/v1/requests?limit=1000";

pub(super) struct Sample {
    pub(super) snapshot: StatsSnapshot,
    pub(super) requests: Option<Requests>,
}

fn read<T: DeserializeOwned>(client: &Client, path: &str, what: &str) -> Result<T, CliError> {
    let response = client.get(path, true)?;
    serde_json::from_slice(&response.body)
        .map_err(|error| CliError::Remote(format!("the {what} is not understood: {error}")))
}

pub(super) fn snapshot(client: &Client) -> Result<StatsSnapshot, CliError> {
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

pub(super) fn sample(client: &Client, with_requests: bool) -> Result<Sample, CliError> {
    Ok(Sample {
        snapshot: snapshot(client)?,
        requests: with_requests
            .then(|| read(client, REQUESTS_PATH, "request list"))
            .transpose()?,
    })
}
