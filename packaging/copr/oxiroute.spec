# Cargo release profile does not emit DWARF debuginfo by default.
%global debug_package %{nil}

Name:           oxiroute
Version:        7.3.0
Release:        1%{?dist}
Summary:        Pingora-based HTTP, TCP, UDP, and RTMP proxy runtime and control plane

License:        Apache-2.0
URL:            https://github.com/brauliobo/oxiroute
Source0:        %{url}/releases/download/v%{version}/oxiroute-%{version}.tar.gz

ExclusiveArch:  x86_64 aarch64

BuildRequires:  cargo
BuildRequires:  rust >= 1.97
BuildRequires:  gcc
BuildRequires:  gcc-c++
BuildRequires:  cmake
BuildRequires:  perl
BuildRequires:  pkgconfig
BuildRequires:  openssl-devel
BuildRequires:  systemd-rpm-macros

Requires:       ca-certificates
Requires(pre):  systemd
%{?systemd_requires}
Suggests:       certbot

%description
OxiRoute is a Rust proxy and control plane for routing HTTP, TCP, UDP, and RTMP
services from one typed configuration. Built on Cloudflare Pingora, it ships a
management CLI (oxiroute, alias oxr), live TUI (oxiroute top), browser dashboard,
and supervised worker launcher.

The package enables and starts nothing: edit /etc/oxiroute/oxiroute.kdl (or
oxiroute.oxi), check it with "oxiroute config check", then run
"systemctl enable --now oxiroute.service".

%prep
%autosetup -n %{name}-%{version}

%build
export OPENSSL_NO_VENDOR=1
cargo build --release --locked \
  --package oxiroute \
  --package oxiroute-supervisor-process

%install
install -Dm755 target/release/oxiroute %{buildroot}%{_bindir}/oxiroute
ln -s oxiroute %{buildroot}%{_bindir}/oxr

install -d -m755 \
  %{buildroot}%{_prefix}/lib/oxiroute \
  %{buildroot}%{_datadir}/bash-completion/completions \
  %{buildroot}%{_datadir}/zsh/site-functions \
  %{buildroot}%{_datadir}/fish/vendor_completions.d \
  %{buildroot}%{_mandir}/man1 \
  %{buildroot}%{_sysconfdir}/oxiroute

install -m755 target/release/oxiroute-worker-launcher \
  %{buildroot}%{_prefix}/lib/oxiroute/oxiroute-worker-launcher
install -m755 packaging/arch/oxiroute.ensure-token \
  %{buildroot}%{_prefix}/lib/oxiroute/ensure-management-token

for name in oxiroute oxr; do
  %{buildroot}%{_bindir}/oxiroute completions bash --name "${name}" \
    > %{buildroot}%{_datadir}/bash-completion/completions/${name}
  %{buildroot}%{_bindir}/oxiroute completions zsh --name "${name}" \
    > %{buildroot}%{_datadir}/zsh/site-functions/_${name}
  %{buildroot}%{_bindir}/oxiroute completions fish --name "${name}" \
    > %{buildroot}%{_datadir}/fish/vendor_completions.d/${name}.fish
  %{buildroot}%{_bindir}/oxiroute man --name "${name}" \
    > %{buildroot}%{_mandir}/man1/${name}.1
done

install -Dm644 packaging/arch/oxiroute.service %{buildroot}%{_unitdir}/oxiroute.service
install -Dm644 packaging/arch/oxiroute.sysusers %{buildroot}%{_sysusersdir}/oxiroute.conf
install -Dm644 packaging/arch/oxiroute.tmpfiles %{buildroot}%{_tmpfilesdir}/oxiroute.conf
install -Dm644 packaging/arch/oxiroute.modules %{buildroot}%{_prefix}/lib/modules-load.d/oxiroute.conf
install -Dm644 packaging/arch/oxiroute.env %{buildroot}%{_sysconfdir}/oxiroute/oxiroute.env
install -Dm644 packaging/arch/oxiroute.kdl %{buildroot}%{_sysconfdir}/oxiroute/oxiroute.kdl
install -Dm644 packaging/arch/oxiroute.lua %{buildroot}%{_sysconfdir}/oxiroute/oxiroute.lua

%pre
%sysusers_create_package oxiroute packaging/arch/oxiroute.sysusers

%post
%tmpfiles_create_package oxiroute packaging/arch/oxiroute.tmpfiles
%{_prefix}/lib/oxiroute/ensure-management-token
%systemd_post oxiroute.service

%preun
%systemd_preun oxiroute.service

%postun
%systemd_postun_with_restart oxiroute.service

%files
%license LICENSE
%doc README.md docs/MANAGEMENT_CLI.md docs/user/INSTALL.md
%{_bindir}/oxiroute
%{_bindir}/oxr
%{_datadir}/bash-completion/completions/oxiroute
%{_datadir}/bash-completion/completions/oxr
%{_datadir}/zsh/site-functions/_oxiroute
%{_datadir}/zsh/site-functions/_oxr
%{_datadir}/fish/vendor_completions.d/oxiroute.fish
%{_datadir}/fish/vendor_completions.d/oxr.fish
%{_mandir}/man1/oxiroute.1*
%{_mandir}/man1/oxr.1*
%dir %{_prefix}/lib/oxiroute
%{_prefix}/lib/oxiroute/ensure-management-token
%{_prefix}/lib/oxiroute/oxiroute-worker-launcher
%{_unitdir}/oxiroute.service
%{_sysusersdir}/oxiroute.conf
%{_tmpfilesdir}/oxiroute.conf
%{_prefix}/lib/modules-load.d/oxiroute.conf
%dir %{_sysconfdir}/oxiroute
%config(noreplace) %{_sysconfdir}/oxiroute/oxiroute.env
%config(noreplace) %{_sysconfdir}/oxiroute/oxiroute.kdl
%config(noreplace) %{_sysconfdir}/oxiroute/oxiroute.lua
%ghost %attr(0600,oxiroute,oxiroute) %{_sysconfdir}/oxiroute/management.token

%changelog
* Sat Nov 28 2026 Braulio Oliveira <brauliobo@gmail.com> - 7.3.0-1
- Initial Fedora COPR source RPM spec for OxiRoute 7.3.0
