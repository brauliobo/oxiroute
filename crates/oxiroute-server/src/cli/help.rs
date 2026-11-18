pub(crate) const EXIT_CATEGORIES: [(u8, &str); 10] = [
    (0, "success"),
    (1, "`serve` could not start the daemon"),
    (2, "usage: an unknown command or a bad option"),
    (
        3,
        "local input: a missing, unreadable or unsafe token or configuration file",
    ),
    (
        4,
        "transport: the daemon is not running, refused the connection or did not answer in time",
    ),
    (
        5,
        "authentication: the token was not accepted or does not allow the operation",
    ),
    (
        6,
        "not found: the pool, server, listener or request does not exist",
    ),
    (
        7,
        "conflict: the revision or state changed under the request",
    ),
    (
        8,
        "remote failure: the daemon refused the request or answered something unexpected",
    ),
    (9, "unsupported: the operation is not available"),
];

pub(crate) const ENVIRONMENT: [(&str, &str); 2] = [
    (
        "OXIROUTE_ENDPOINT",
        "management API URL, default http://127.0.0.1:9900 (the packaged setup is read when it is unset)",
    ),
    (
        "OXIROUTE_MANAGEMENT_TOKEN_FILE",
        "file holding the management bearer token, default /etc/oxiroute/management.token",
    ),
];

pub(crate) const FILES: [(&str, &str); 3] = [
    (
        "/etc/oxiroute/oxiroute.kdl",
        "packaged configuration; its management bind is the default endpoint",
    ),
    (
        "/etc/oxiroute/oxiroute.env",
        "packaged environment file, read for the two variables above",
    ),
    (
        "/etc/oxiroute/management.token",
        "default management token file",
    ),
];

macro_rules! getting_started {
    () => {
        "Getting started:
  oxiroute serve /etc/oxiroute/oxiroute.kdl   run the daemon with a configuration file
  oxiroute config check oxiroute.kdl          check a configuration file without a daemon
  oxiroute status                             health, active revision and listeners
  oxiroute pool list                          pools and how many servers are available
  oxiroute server drain --pool web app-1      take a server out of rotation
  oxiroute generation reload --wait           apply the configuration file and wait for it
  oxiroute top                                live dashboard

`oxiroute CONFIG` is shorthand for `oxiroute serve CONFIG`. Every command takes --help; `oxiroute man`
prints the manual page and `oxiroute completions bash|zsh|fish` the shell completions."
    };
}

pub(crate) const ABOUT: &str = "Run the OxiRoute proxy and operate it while it serves.";

pub(crate) const LONG_ABOUT: &str = "Run the OxiRoute proxy and operate it while it serves.

`serve` runs the daemon from a configuration file. The other commands talk to its management API on \
loopback: inspect pools, servers, listeners and requests, drain and re-weight servers without a restart, \
reload or roll back the configuration, and import native nginx, HAProxy, Squid, Apache and Varnish files.";

pub(crate) const GETTING_STARTED: &str = getting_started!();

pub(crate) const OVERVIEW: &str = concat!(
    getting_started!(),
    "

Exit status (except `check`, which exits 0 OK, 1 WARNING, 2 CRITICAL, 3 UNKNOWN for monitoring systems):
  0  success
  1  `serve` could not start the daemon
  2  usage: an unknown command or a bad option
  3  local input: a missing, unreadable or unsafe token or configuration file
  4  transport: the daemon is not running, refused the connection or did not answer in time
  5  authentication: the token was not accepted or does not allow the operation
  6  not found: the pool, server, listener or request does not exist
  7  conflict: the revision or state changed under the request
  8  remote failure: the daemon refused the request or answered something unexpected
  9  unsupported: the operation is not available"
);

pub(crate) const SERVER_EXAMPLES: &str = "Examples:
  oxiroute server list --pool web
  oxiroute server drain --pool web app-1
  oxiroute server weight set --pool web app-1 50
  oxiroute server register --pool web app-9 --address 192.0.2.10:8080 --ttl 15s --heartbeat";

pub(crate) const POOL_EXAMPLES: &str = "Examples:
  oxiroute pool list
  oxiroute pool show web
  oxiroute pool drain web";

pub(crate) const LISTENER_EXAMPLES: &str = "Examples:
  oxiroute listener list
  oxiroute listener drain edge
  oxiroute listener ready edge";

pub(crate) const GENERATION_EXAMPLES: &str = "Examples:
  oxiroute generation status
  oxiroute generation reload --wait --timeout 60s
  oxiroute generation rollback";

pub(crate) const CONFIG_EXAMPLES: &str = "Examples:
  oxiroute config check /etc/oxiroute/oxiroute.kdl
  oxiroute config get
  oxiroute --output json config get > active.json
  oxiroute config diff active.json

`config validate`, `config apply` and `config diff` read canonical JSON, not a native kdl, lua or hocon file; \
`config check` reads any native format.";

pub(crate) const TLS_EXAMPLES: &str = "Examples:
  oxiroute tls list
  oxiroute tls reconcile --certificate www";

pub(crate) const EVENTS_EXAMPLES: &str = "Examples:
  oxiroute events list --limit 20
  oxiroute events follow";

pub(crate) const REQUESTS_EXAMPLES: &str = "Examples:
  oxiroute requests list --older-than 30s
  oxiroute requests show REQUEST_ID";

pub(crate) const RTMP_EXAMPLES: &str = "Examples:
  oxiroute rtmp stream list
  oxiroute rtmp recorder start STREAM_ID RECORDER";

pub(crate) const REGISTRY_EXAMPLES: &str = "Examples:
  oxiroute registry token create --tokens-file tokens.json --id deploy --pool web";

pub(crate) const IMPORT_EXAMPLES: &str = "Examples:
  oxiroute import nginx /etc/nginx/nginx.conf
  oxiroute import nginx /etc/nginx/nginx.conf --emit preview --format kdl";

#[cfg(test)]
mod tests;
