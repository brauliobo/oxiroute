# Getting Started

This guide uses the repository's complete KDL example. It gives you one reverse HTTP proxy, one
opaque TCP relay, one RTMP listener, and the loopback management UI.

## Requirements

- Rust `1.97.1` or a compatible toolchain for the checked-in release line.
- Node.js and pnpm `11.3.0` for the dashboard build.
- OpenSSL for the local token-generation example.
- A Linux host for the full monitoring and supervision surface. Unprivileged local listeners work on
  other supported Rust platforms where the relevant transport exists.

## Run The Example

Start an origin on `127.0.0.1:3000` that answers `GET /healthz` with `200`. A small Python origin is
included in the [top-level quickstart](../../README.md#five-minute-local-run).

Build the UI and create a restrictive token file:

```sh
pnpm --dir ui install
pnpm --dir ui build
umask 077
openssl rand -hex 32 > /tmp/oxiroute-management.token
```

Validate the configuration before starting the daemon:

```sh
cargo run -p oxiroute -- config check oxiroute.example.kdl
```

Start it with the token file in the environment:

```sh
OXIROUTE_MANAGEMENT_TOKEN_FILE=/tmp/oxiroute-management.token \
  cargo run -p oxiroute -- serve oxiroute.example.kdl
```

The `management` object in the example binds `127.0.0.1:9080` and serves `ui/dist`. The HTTP proxy
binds `127.0.0.1:8080` and uses an active HTTP health check against `/healthz` on port `3000`. This
example is HTTP/1-only; H2 and H3 listener shapes are documented in the
[configuration specification](../CONFIG_SPEC.md).

## Verify It

```sh
TOKEN=$(tr -d '\r\n' < /tmp/oxiroute-management.token)
curl -i http://127.0.0.1:8080/healthz
curl -s -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:9080/api/v1/monitoring | jq .
curl -s -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:9080/api/v1/topology | jq '.state, (.nodes | length), (.edges | length)'
```

Open `http://127.0.0.1:9080/` for the dashboard. The first page is the runtime observatory. The
Statistics view is the HAProxy-oriented table; Configuration asks for the bearer token and retains
it only in page memory.

## Make A First Change

Copy the example before editing it. Run `config check` against the copy, then use a revision-aware
management operation or restart the local process. Do not edit the packaged or production file while
another editor may be saving it without understanding disk revisions.

```sh
cp oxiroute.example.kdl /tmp/oxiroute-local.kdl
cargo run -p oxiroute -- config check /tmp/oxiroute-local.kdl
```

For a real management client, set the endpoint and token explicitly:

```sh
export OXIROUTE_ENDPOINT=http://127.0.0.1:9080
export OXIROUTE_MANAGEMENT_TOKEN_FILE=/tmp/oxiroute-management.token
oxiroute status
oxiroute monitoring
oxiroute generation status
```

## Package Installation

The repository contains an AUR-ready Arch Linux recipe under `packaging/arch`. It installs the
daemon, offline importer, management client, systemd metadata, examples, and the `oxr` alias. The
package does not enable or start the service automatically; review the installed configuration first.
See [the packaging notes](../../packaging/arch/README.md) for permissions, token setup, recording
roots, and systemd drop-ins.

## Next Reading

- [Dashboard guide](DASHBOARD.md) for the browser's views and data boundaries.
- [Operations guide](OPERATING.md) for production-shaped lifecycle actions.
- [Configuration reference](../reference/CONFIGURATION.md) for the smallest useful objects.
