#[path = "../../support/config.rs"]
mod config_support;
#[path = "../../support/fixtures.rs"]
mod fixture_support;
#[path = "../../support/http.rs"]
mod http_support;
#[path = "../../support/ports.rs"]
mod ports_support;

mod access;
mod affinity;
mod balancing;
mod bodies;
mod cache;
mod endpoints;
mod files;
mod fixed;
mod gzip;
mod headers;
mod identity;
mod logging;
mod rescue;
mod retries;
mod reuse;
mod routing;
mod selectors;
mod support;
mod timeouts;
mod unavailable;

use std::time::Duration;

const TEST_TIMEOUT: Duration = Duration::from_secs(30);
