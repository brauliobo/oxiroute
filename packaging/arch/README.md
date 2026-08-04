# Arch Linux packaging

This directory is the AUR-ready release recipe for `oxiroute`. It builds the locked Rust
workspace packages with the upstream release profile (`codegen-units = 1`, fat LTO), then installs
the unified `oxiroute` daemon, offline importer, and management client, the `oxr` short alias,
the staged `/usr/lib/oxiroute/oxiroute-worker-launcher`, Apache-2.0 license, systemd service,
sysusers/tmpfiles metadata, management documentation, and administrator-owned KDL and Lua
configuration examples.

The package installs but does not enable or start the service. Review `/etc/oxiroute/oxiroute.kdl`,
then use `systemctl enable --now oxiroute.service` when it is ready.
The unit executes `oxiroute serve /etc/oxiroute/oxiroute.kdl`; validate edits offline with
`oxiroute config check /etc/oxiroute/oxiroute.kdl`. The installed `/etc/oxiroute/oxiroute.lua`
retains the equivalent restricted-Lua compatibility example but is not the service default.
When upgrading from a Lua-default package, the package hook atomically converts the existing Lua
configuration if the newly installed KDL file is still the packaged empty default. If conversion
fails, a systemd drop-in keeps the service on Lua instead of silently starting the empty KDL root.
The shipped secret-free examples are root-owned and world-readable so the dynamic service account can
read them on filesystems without POSIX ACL support. Tighten them as described below when adding secrets.

The launcher is staged for the future supervised master at
`/usr/lib/oxiroute/oxiroute-worker-launcher`; installing it does not activate supervised mode. The
service keeps the current direct `oxiroute serve /etc/oxiroute/oxiroute.kdl` entry point, while
supervised-mode dispatch remains behind a future runtime activation gate for an explicitly supported
configuration set. Do not add a service override to select supervised mode until that gate is wired
and documented.

To enable the authenticated management client, create the restrictive default token file:

```sh
umask 077
openssl rand -hex 32 > /etc/oxiroute/management.token
chown oxiroute:oxiroute /etc/oxiroute/management.token
chmod 600 /etc/oxiroute/management.token
```

Add the management object:

```kdl
(object)management {
  bind "127.0.0.1:9900"
}
```

The package environment file has a commented example for a configured token path. Uncomment or
change that assignment when the daemon should use a non-default path. The client resolves paths in
this order: explicit `--token-file`, process `OXIROUTE_MANAGEMENT_TOKEN_FILE`, the one plain
assignment in `/etc/oxiroute/oxiroute.env`, then `/etc/oxiroute/management.token` when present. It
does not execute the environment file. With the default token above, no token option or environment
export is needed:

```sh
oxiroute status
```

The daemon still needs `OXIROUTE_MANAGEMENT_TOKEN_FILE` in its systemd environment file whenever a
management listener is configured; uncomment the packaged assignment before starting the service.
The CLI's default-path discovery is a client convenience and does not change that startup gate.

With the authenticated management listener enabled, `systemctl reload oxiroute.service` runs
`/usr/bin/oxiroute generation reload`. The command re-reads the canonical configuration and
re-resolves imported native dependencies; it fails rather than performing a no-op when the
management endpoint or token is unavailable. Candidate activation is asynchronous, so use
`oxiroute generation status` to confirm the new generation is active. A rejected candidate leaves
the current generation active and available for `oxiroute generation rollback`.

## Release source

`PKGBUILD` expects the checksum-pinned release asset
`https://github.com/brauliobo/oxiroute/releases/download/v0.3.0/oxiroute-0.3.0.tar.gz`. The asset is a
deterministic archive of Git-tracked files with the `oxiroute-0.3.0/` prefix and without this
`packaging/arch` directory. Excluding untracked and ignored files prevents local build or benchmark
artifacts from entering a release. Excluding the AUR recipe avoids making the source checksum depend
on the checksum recorded inside the recipe.

The v0.3.0 archive includes the supervised launcher and related workspace crates. Publish the release
archive before using the recipe, then keep the source checksum synchronized with that exact asset and
regenerate `.SRCINFO`.

From this repository, `./build-local.sh` recreates an archive from the current worktree and verifies it
against the PKGBUILD checksum. Until the next release archive is published, that generated archive
intentionally does not match the pinned v0.2.3 checksum because the current worktree contains the
unreleased supervisor crates. Pass an existing matching archive as the first argument to verify and
use a release artifact instead. Extra arguments are passed to `makepkg`.

Examples:

```sh
./build-local.sh
./build-local.sh /path/to/oxiroute-0.3.0.tar.gz --cleanbuild
```

Build products and source archives are written below `.makepkg/`, which is ignored by Git. Cargo's
locked dependencies are fetched during `prepare()`; `build()` then uses `--frozen` with network
access disabled and at most four jobs. To use a pre-populated Cargo cache in an offline build
environment, set `CARGO_HOME` before invoking the helper.

For a normal AUR build after publishing the release asset:

```sh
makepkg --cleanbuild
```

Every source and package artifact is checksum-pinned. Regenerate `.SRCINFO` whenever those hashes
change.

## Service access

The packaged empty configuration binds the read-only operational endpoint to
`127.0.0.1:8404`. Public readiness is `http://127.0.0.1:8404/ready`, and public Prometheus
exposition is `http://127.0.0.1:8404/metrics`. The packaged `admin_token_file #null` intentionally
leaves `/stats`, `/api/v1/status`, and `POST /stats/admin` inaccessible. To enable them, create a
mode-`0600` token owned by `oxiroute`, set `stats.admin_token_file` to its path, and send
`Authorization: Bearer <token>` from loopback. Administration also requires the active
`If-Generation-Revision`; GET and HEAD never mutate state.

`oxr` is an exact symlink alias for `oxiroute`; all commands and options are identical.

The service runs in the foreground as the static `oxiroute` user and group. It receives only
`CAP_NET_BIND_SERVICE`, retains normal IPv4/IPv6/Unix networking for DNS and upstream traffic, and
can read normal filesystem paths. `ProtectSystem=strict` makes the filesystem read-only except for
`/run/oxiroute` and `/var/lib/oxiroute`; the packaged RTMP recording root is
`/var/lib/oxiroute/recordings`.

Certbot lineages and static roots require no filesystem sandbox override. Grant the `oxiroute` user
Unix read/search permission, preferably through a purpose-specific group. If a local group owns a
private tree, add it without replacing the primary group:

```ini
[Service]
SupplementaryGroups=certificates
```

For an RTMP recording root outside `/var/lib/oxiroute`, grant Unix write/search permission and add a
drop-in such as:

```ini
[Service]
ReadWritePaths=/srv/oxiroute-recordings
```

The authenticated configuration API atomically replaces its configuration file. To enable writes
to the packaged `/etc` location, grant the directory to the service group and add:

```ini
[Service]
ReadWritePaths=/etc/oxiroute
```

For example, use a local tmpfiles override to set `/etc/oxiroute` to `0770 root:oxiroute`; keep the
configuration at `0640 root:oxiroute` and the management token at `0600 oxiroute:oxiroute`. Set
`OXIROUTE_MANAGEMENT_TOKEN_FILE=/etc/oxiroute/management.token` in the environment file only after
creating that token with restrictive permissions.

After changing a drop-in, run `systemctl daemon-reload` and restart the service. Do not add writable
access for Certbot or static-content trees unless the configured application explicitly needs to
modify them.
