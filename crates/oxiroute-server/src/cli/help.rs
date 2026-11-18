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

pub(crate) const ENVIRONMENT: [(&str, &str); 3] = [
    (
        "OXIROUTE_ENDPOINT",
        "management API URL, default http://127.0.0.1:9900 (the packaged setup is read when it is unset)",
    ),
    (
        "OXIROUTE_CONTEXT",
        "name of the context from the contexts file whose address, token source and timeout apply; the flags and the two variables above take precedence",
    ),
    (
        "OXIROUTE_MANAGEMENT_TOKEN_FILE",
        "file holding the management bearer token, default /etc/oxiroute/management.token",
    ),
];

pub(crate) const FILES: [(&str, &str); 5] = [
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
    (
        "$XDG_CONFIG_HOME/oxiroute/contexts",
        "named contexts (mode 0600), managed with `oxiroute context`; ~/.config is used when XDG_CONFIG_HOME is unset",
    ),
    (
        "<configuration file>.overlay.json",
        "runtime changes kept with `--persist` (mode 0600), managed with `oxiroute overlay`",
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

Exit status (except `check` and `tls check`, which exit 0 OK, 1 WARNING, 2 CRITICAL, 3 UNKNOWN for monitoring systems):
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
  oxiroute server drain --pool web app-1 --persist
  oxiroute server register --pool web app-9 --address 192.0.2.10:8080 --ttl 15s --heartbeat";

pub(crate) const OVERLAY_EXAMPLES: &str = "Examples:
  oxiroute server maintenance --pool web app-1 --persist
  oxiroute overlay show
  oxiroute overlay promote --wait
  oxiroute overlay clear

`--persist` keeps a runtime change in the overlay file next to the configuration (`oxiroute.kdl.overlay.json`), \
which the daemon applies again at every start and reload. The configuration file itself is not edited until \
`overlay promote`.";

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
  oxiroute config show > oxiroute.kdl
  oxiroute config diff --text oxiroute.kdl
  oxiroute config apply --wait oxiroute.kdl
  oxiroute config edit
  oxiroute config history
  oxiroute config diff --rev 3f9a1c2b7d40
  oxiroute config rollback 3f9a1c2b7d40 --wait

`config validate`, `config diff` and `config apply` take a file in any native format (kdl, lua, hocon, uci, or a JSON \
document), chosen by its extension or `--format`, and ask the daemon, so they check what `config check` checks offline \
plus the running state. `config apply` stores the file's own text, with its comments and Lua logic, when it is in the \
format of the daemon's file. Exit codes: 3 for an invalid file, 7 when the file changed on disk meanwhile, and 1 for \
differences with `config diff --exit-code` or `config fmt --check`.";

pub(crate) const CONTEXT_EXAMPLES: &str = "Examples:
  oxiroute context add prod --address http://192.0.2.10:9900 --token-from-file /etc/oxiroute/prod.token --use
  oxiroute context add staging --address http://192.0.2.20:9900 --token-from-env STAGING_TOKEN
  oxiroute context list
  oxiroute --context staging status

Precedence, highest first: flags (--endpoint, --token-file, --timeout), the variables (OXIROUTE_ENDPOINT, \
OXIROUTE_MANAGEMENT_TOKEN_FILE), the context (--context, OXIROUTE_CONTEXT, then the current one), the packaged \
setup. The contexts file holds addresses and where a token comes from, never a token, and must have mode 0600.";

pub(crate) const TLS_EXAMPLES: &str = "Examples:
  oxiroute tls list --expiring-within 30d
  oxiroute tls show www
  oxiroute tls inspect example.com:443
  oxiroute tls check --warn 21d --crit 7d
  oxiroute tls check example.com:443 --warn 21d
  oxiroute tls renew --certificate www
  oxiroute tls reconcile --certificate www

`tls check` exits 0 OK, 1 WARNING, 2 CRITICAL, 3 UNKNOWN, like `oxiroute check`.";

pub(crate) const EVENTS_EXAMPLES: &str = "Examples:
  oxiroute events list --limit 20
  oxiroute events follow
  oxiroute events follow --since 10m --count 50
  oxiroute --output json events follow --after 0

`events follow` reads the event stream, reconnects from the last event it printed, and ends with status 130 on Ctrl-C.";

pub(crate) const AUDIT_EXAMPLES: &str = "Examples:
  oxiroute audit list --since 1h --category control
  oxiroute audit list --result rejected --limit 20
  oxiroute audit status
  oxiroute audit tail --interval 2s";

pub(crate) const REQUESTS_EXAMPLES: &str = "Examples:
  oxiroute requests list --older-than 30s
  oxiroute requests show REQUEST_ID";

pub(crate) const RTMP_EXAMPLES: &str = "Examples:
  oxiroute rtmp stream list
  oxiroute rtmp stream disconnect STREAM_ID
  oxiroute rtmp publisher list
  oxiroute rtmp viewer kick SESSION_ID
  oxiroute rtmp recorder start STREAM_ID RECORDER
  oxiroute rtmp recording list --stream camera --status complete
  oxiroute rtmp recording download RECORDING_ID --to camera.flv
  oxiroute rtmp recording quota";

pub(crate) const REGISTRY_EXAMPLES: &str = "Examples:
  oxiroute registry token create --tokens-file tokens.json --id deploy --pool web";

pub(crate) const IMPORT_EXAMPLES: &str = "Examples:
  oxiroute import nginx /etc/nginx/nginx.conf
  oxiroute import nginx /etc/nginx/nginx.conf --emit preview --format kdl";

#[cfg(test)]
mod tests;
