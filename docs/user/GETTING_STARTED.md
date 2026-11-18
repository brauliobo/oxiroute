# Run a local proxy

Run one verified HTTP request through the checked-in example, then inspect the daemon in the dashboard.
This is a local evaluation, not a production deployment. The example also opens TCP and RTMP listeners.
All commands below run from the root of a source checkout.

## Requirements

- Linux for the full supported build, monitoring, and supervision path. Other platforms do not have
  the same full-build CI gate; see [compatibility](../COMPATIBILITY.md).
- Rust `1.97.1`, the toolchain tested in CI. The workspace declares Rust `1.97` as its minimum.
- A native C/C++ build toolchain, CMake, pkg-config, and OpenSSL development headers/libraries for
  the native dependencies. Package names depend on your distribution.
- Node.js `22` and pnpm `11.3.0`, matching the dashboard CI setup.
- Python 3, OpenSSL, and curl for this walkthrough; jq for formatted API output.
- Free loopback ports `3000`, `8080`, `15432`, `1935`, and `9080`.

## 1. Start a local origin

In terminal A, create a temporary directory containing a health response and start Python's HTTP server:

```sh
demo_dir=$(mktemp -d)
printf 'ok\n' > "$demo_dir/healthz"
python3 -m http.server 3000 --bind 127.0.0.1 --directory "$demo_dir"
```

Leave it running. The proxy checks `GET /healthz` on this origin before selecting it.

## 2. Build and validate

In terminal B, from the repository root:

```sh
pnpm --dir ui install --frozen-lockfile
nice pnpm --dir ui build
nice cargo +1.97.1 build --locked --jobs 4 -p oxiroute
```

Create a private temporary directory and token file, validate, then start the daemon:

```sh
session_dir=$(mktemp -d)
chmod 700 "$session_dir"
umask 077
openssl rand -hex 32 > "$session_dir/management.token"
export OXIROUTE_MANAGEMENT_TOKEN_FILE="$session_dir/management.token"
printf 'Token file: %s\n' "$OXIROUTE_MANAGEMENT_TOKEN_FILE"
./target/debug/oxiroute config check oxiroute.example.kdl
./target/debug/oxiroute serve oxiroute.example.kdl
```

A successful check prints `configuration is valid and runtime-preparable`. It loads and prepares the
configuration locally, including listener reservations, without serving traffic. The example ports
must be free during this check. If reservation fails, use an unused bind in a copy of the example;
do not stop an unrelated service. Ports must still be free when the daemon starts.

Leave terminal B running. The token authenticates management operations and is unrelated to the
HTTP proxy listener. Keep it out of source control and support reports.

| Listener | Address | What this example exercises |
| --- | --- | --- |
| HTTP | `127.0.0.1:8080` | Reverse HTTP/1 proxy to the origin on port `3000` |
| TCP | `127.0.0.1:15432` | Relay to port `5432`; no database is needed for the HTTP walkthrough |
| RTMP | `127.0.0.1:1935` | Configured live media listener; no publisher is needed here |
| Management/UI | `127.0.0.1:9080` | Dashboard and authenticated management API |

The source build runs directly unless an eligible Linux supervision launcher is installed. See
[operations](OPERATING.md) for the runtime modes.

## 3. Verify traffic and inspect state

In terminal C, from the repository root:

```sh
curl -i http://127.0.0.1:3000/healthz
curl -i http://127.0.0.1:8080/healthz
./target/debug/oxiroute --endpoint http://127.0.0.1:9080 ready --wait --timeout 30s
```

Both HTTP requests should return `200` with an `ok` body. Wait for the first health check if the
proxy initially returns `503`. If it persists, inspect the origin and pool health using
[troubleshooting](TROUBLESHOOTING.md).

Open `http://127.0.0.1:9080/`. Read the token file locally and enter its contents when prompted;
the dashboard keeps the token in page memory. For CLI access, use the **token-file path** printed in
terminal B as the value below:

```sh
export OXIROUTE_ENDPOINT=http://127.0.0.1:9080
export OXIROUTE_MANAGEMENT_TOKEN_FILE=/path/to/management.token
./target/debug/oxiroute status
./target/debug/oxiroute --output json monitoring | jq .
./target/debug/oxiroute generation status
```

The explicit endpoint matters: the CLI default is port `9900`, while this example uses `9080`.
Readiness and metrics probes need no token; recognized management API routes do. `oxiroute --help` lists
the commands with a getting-started section and the exit codes; each command has its own `--help` with
examples.

## 4. Make a first change

Stop the local daemon in terminal B with Ctrl-C so the listener ports are free. In terminal C,
with the token-file environment still set, copy the example, edit the copy, and validate it:

```sh
cp oxiroute.example.kdl /tmp/oxiroute-local.kdl
./target/debug/oxiroute config check /tmp/oxiroute-local.kdl
```

For a local experiment, start the daemon with the copy from terminal B. For live configuration changes,
follow [operations](OPERATING.md): validation, disk revisions, and generation publication are
separate steps. A failed candidate leaves the active generation serving.

KDL 2.0 is recommended for new configurations. The [configuration reference](../reference/CONFIGURATION.md)
contains a smaller HTTP-only shape; the [configuration specification](../CONFIG_SPEC.md) defines
TLS, HTTP/2, HTTP/3, defaults, and limits.

## Stop and clean up

Stop the daemon in terminal B and the origin in terminal A with Ctrl-C. In terminal B, remove only
the temporary session created above:

```sh
rm "$session_dir/management.token"
rmdir "$session_dir"
unset OXIROUTE_MANAGEMENT_TOKEN_FILE
```

In terminal A:

```sh
rm "$demo_dir/healthz"
rmdir "$demo_dir"
```

Remove `/tmp/oxiroute-local.kdl` if you created the copy and no longer need it.

## Install a package or continue

Releases attach Debian and RPM packages and binary tarballs ([install a release package](INSTALL.md)), and the
repository includes an Arch Linux package recipe. They install the daemon, importer, management client, systemd
metadata and the `oxr` alias, and do not start or enable the service. Review
[packaging instructions](../../packaging/arch/README.md) before installing the Arch package.

- [Operations](OPERATING.md): health, reloads, generations, and drains.
- [Dashboard](DASHBOARD.md): live telemetry and configuration state.
- [Security](SECURITY.md): trust boundaries and token permissions.
- [Benchmarks](../BENCHMARKS.md): measured workloads and limits of the evidence.
