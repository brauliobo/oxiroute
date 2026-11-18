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
