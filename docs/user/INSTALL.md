# Install a release package

OxiRoute publishes hosted **APT** and **RPM** package repositories on GitHub Pages
([`brauliobo.github.io/oxiroute/repos`](https://brauliobo.github.io/oxiroute/repos/)), AUR recipes (`oxiroute-bin`,
`oxiroute`, `oxiroute-git`), a Fedora COPR spec (`packaging/copr/oxiroute.spec`), and direct `.deb`, `.rpm`,
`.pkg.tar.zst`, and tarball assets with one `SHA256SUMS` on every
[GitHub release](https://github.com/brauliobo/oxiroute/releases).

## Hosted repositories

### Debian and Ubuntu (APT — `amd64` & `arm64`)

```sh
echo "deb [trusted=yes] https://brauliobo.github.io/oxiroute/apt ./" | sudo tee /etc/apt/sources.list.d/oxiroute.list
sudo apt update && sudo apt install oxiroute
```

### Fedora, RHEL, AlmaLinux, Rocky Linux, Amazon Linux 2023, and openSUSE (RPM — `x86_64` & `aarch64`)

```sh
# Fedora / RHEL / AlmaLinux / Rocky Linux
curl -fsSL https://brauliobo.github.io/oxiroute/rpm/oxiroute.repo | sudo tee /etc/yum.repos.d/oxiroute.repo
sudo dnf install oxiroute

# Amazon Linux 2023
curl -fsSL https://brauliobo.github.io/oxiroute/rpm/amzn2023/oxiroute.repo | sudo tee /etc/yum.repos.d/oxiroute.repo
sudo dnf install oxiroute

# openSUSE
sudo zypper addrepo --no-gpgcheck https://brauliobo.github.io/oxiroute/rpm oxiroute
sudo zypper refresh && sudo zypper install --allow-unsigned-rpm oxiroute
```

### Arch Linux / CachyOS (AUR)

```sh
paru -S oxiroute-bin     # prebuilt PGO binary
paru -S oxiroute         # build from release source
paru -S oxiroute-git     # build from main
```

## Choose a direct release asset

| Platform | Asset | Architectures |
| --- | --- | --- |
| Debian 12 and 13, Ubuntu 22.04 and 24.04 | `oxiroute_<version>_<arch>.deb` | `amd64`, `arm64` |
| RHEL-family 9 (AlmaLinux, Rocky Linux; also installs on AlmaLinux 10) | `oxiroute-<version>-1.el9.<arch>.rpm` | `x86_64`, `aarch64` |
| Amazon Linux 2023 | `oxiroute-<version>-1.amzn2023.<arch>.rpm` | `x86_64`, `aarch64` |
| Arch Linux | `oxiroute-<version>-1-x86_64.pkg.tar.zst` | `x86_64` |
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

## Arch Linux

```sh
sudo pacman -U ./oxiroute-<version>-1-x86_64.pkg.tar.zst
```

The package is named `oxiroute`, so a pacman repository that serves the file replaces and upgrades it like any other
package. The AUR package `oxiroute-bin` installs this same file (`oxiroute` on the AUR builds from source and has the same
name, so the two replace each other). The Arch package is x86_64 only.

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
| `/etc/oxiroute/management.token` | Management bearer token (`0600 oxiroute:oxiroute`), generated on install/upgrade if absent |
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
- **Upgrading from 7.0.x** to 7.1.0 changes a few CLI spellings (`import --emit`, `generation drain --wait`) and
  adds `NotifyAccess=main` to the unit. Read the [upgrade notes](../RELEASE_NOTES_7.1.0.md#upgrading); a running
  daemon can take the new binary without dropping connections with `oxiroute upgrade --wait`.
- **Upgrading from 7.1.0** to 7.1.1 needs no configuration change. `oxiroute top --json` prints a new schema and
  client and connection tracking is on by default; read the [upgrade notes](../RELEASE_NOTES_7.1.1.md#upgrading).
- **Upgrading from 7.1.1** to 7.2.0 needs no configuration change. YAML 1.2, TOML, and sandboxed Lua 5.4 scripting are
  available, and the package creates `/etc/oxiroute/management.token` (`0600 oxiroute:oxiroute`) when missing; read the
  [upgrade notes](../RELEASE_NOTES_7.2.0.md#upgrading).
- **Upgrading from 7.2.0** to 7.3.0 needs no configuration change. Multi-file workspace editing, format conversion,
  declarative CLI resource CRUD, and full 7-format UI/API parity are additive; read the
  [upgrade notes](../RELEASE_NOTES_7.3.0.md#upgrading).
- **Remove** (`apt remove oxiroute`, `dnf remove oxiroute`) deletes the program and keeps your
  configuration (rpm keeps a modified file as `<file>.rpmsave`) and `/var/lib/oxiroute`.
- **Purge** (`apt purge oxiroute`) also deletes `/etc/oxiroute`, `/var/lib/oxiroute` and `/run/oxiroute`. The
  `oxiroute` user stays, as Debian policy recommends for system users that may still own files; rpm has no purge,
  delete `/var/lib/oxiroute` yourself after `dnf remove`.
