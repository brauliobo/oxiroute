# Install a release package

Every release attaches Debian and RPM packages and per-architecture binary tarballs to its
[GitHub release](https://github.com/brauliobo/oxiroute/releases), with one `SHA256SUMS` for all assets.
There is no hosted apt or yum repository: download the file, verify it, install it from disk.

## Choose an asset

| Platform | Asset | Architectures |
| --- | --- | --- |
| Debian 12 and 13, Ubuntu 22.04 and 24.04 | `oxiroute_<version>_<arch>.deb` | `amd64`, `arm64` |
| RHEL-family 9 (AlmaLinux, Rocky Linux; also installs on AlmaLinux 10) | `oxiroute-<version>-1.el9.<arch>.rpm` | `x86_64`, `aarch64` |
| Amazon Linux 2023 | `oxiroute-<version>-1.amzn2023.<arch>.rpm` | `x86_64`, `aarch64` |
| Any other glibc Linux | `oxiroute-<version>-<arch>-unknown-linux-gnu.tar.gz` | `x86_64`, `aarch64` |

Replace `<version>` with the release version without the leading `v` (for example `7.0.0`).

## Verify the download

Download the asset and `SHA256SUMS` from the same release into one directory and check only the file you
downloaded:

```sh
sha256sum --check --ignore-missing SHA256SUMS
```

The source archive, the tarballs and the packages are also attested by the release workflow. With the GitHub CLI:

```sh
gh attestation verify <asset> --repo brauliobo/oxiroute
```

## Debian and Ubuntu

```sh
sudo apt install ./oxiroute_<version>_amd64.deb
```

## RHEL-family 9 and Amazon Linux 2023

```sh
sudo dnf install ./oxiroute-<version>-1.el9.x86_64.rpm
sudo dnf install ./oxiroute-<version>-1.amzn2023.x86_64.rpm
```

`yum install` takes the same file names on these systems. Use the `aarch64` file on 64-bit Arm hosts.

## Binary tarball

```sh
tar -xzf oxiroute-<version>-x86_64-unknown-linux-gnu.tar.gz
sudo install -m 0755 oxiroute-<version>-x86_64-unknown-linux-gnu/bin/oxiroute /usr/local/bin/oxiroute
oxiroute version
```

The tarball holds `bin/oxiroute`, the `bin/oxr` alias, `lib/oxiroute/oxiroute-worker-launcher`, the license and
`BUILD-INFO.json`. It carries no service files; use a package, or the unit in `packaging/arch/`, for a service. The
tarballs and the packages hold the same binary, built against glibc 2.34, so it runs on any glibc Linux of that
version or newer (the `abort` and `x86-64-v3` flavours are tarballs only).

## What the packages install

| Path | Content |
| --- | --- |
| `/usr/bin/oxiroute`, `/usr/bin/oxr` | The daemon and management client; `oxr` is an alias |
| `/usr/lib/oxiroute/oxiroute-worker-launcher` | The supervised-worker launcher |
| `/usr/lib/systemd/system/oxiroute.service` | The systemd unit |
| `/usr/lib/sysusers.d/oxiroute.conf`, `/usr/lib/tmpfiles.d/oxiroute.conf` | The `oxiroute` system user and `/var/lib/oxiroute` |
| `/usr/lib/modules-load.d/oxiroute.conf` | Loads the `tls` kernel module for kernel TLS offload |
| `/etc/oxiroute/oxiroute.kdl`, `oxiroute.env`, `oxiroute.lua` | Configuration files, never overwritten by an upgrade |
| `/usr/share/doc/oxiroute/` | This guide, the management CLI guide, `BUILD-INFO.json` and the license |

The package depends on glibc, `systemd` and `ca-certificates`; OpenSSL is linked statically. Kernel TLS and
io_uring need kernel features, not packages. The unit uses cgroup delegation directives introduced by
systemd 254 and 257 (`DelegateSubgroup=`, `ProtectControlGroups=private`); an older systemd, as on Debian 12,
Ubuntu 22.04 and 24.04, logs that it ignores them and the unit otherwise loads unchanged.

## Start the service

The package creates the `oxiroute` user and its state directory but does not enable or start anything.
The installed configuration defines no listener or service, only the statistics endpoint on `127.0.0.1:8404`.
Edit it, check it, then opt in:

```sh
sudoedit /etc/oxiroute/oxiroute.kdl
oxiroute config check /etc/oxiroute/oxiroute.kdl
sudo systemctl enable --now oxiroute.service
```

See [operations](OPERATING.md) for reloads and drains and [security](SECURITY.md) for the management token.

## Upgrade, remove, purge

- **Upgrade** with the same install command on the newer file. A modified `/etc/oxiroute` file is kept; if the
  new package ships a different default, dpkg asks (keep yours) and rpm writes `<file>.rpmnew` next to it. A
  running service is restarted by the package manager on upgrade, as the distribution policy requires.
- **Upgrading from 6.x** to 7.0.0 changes four defaults (the HTTP/1 data plane, the TLS backend, the HTTP/3
  congestion controller and the allocator's huge-page policy); each has a setting that restores the old behaviour. Read
  the [upgrade guide](../RELEASE_NOTES_7.0.0.md#upgrade-guide-from-65x) before restarting a production service.
- **Remove** (`apt remove oxiroute`, `dnf remove oxiroute`) deletes the program and keeps your
  configuration (rpm keeps a modified file as `<file>.rpmsave`) and `/var/lib/oxiroute`.
- **Purge** (`apt purge oxiroute`) also deletes `/etc/oxiroute`, `/var/lib/oxiroute` and `/run/oxiroute`. The
  `oxiroute` user stays, as Debian policy recommends for system users that may still own files; rpm has no purge,
  delete `/var/lib/oxiroute` yourself after `dnf remove`.
