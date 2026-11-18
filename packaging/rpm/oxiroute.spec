%global debug_package %{nil}
%global _build_id_links none

Name:           oxiroute
Version:        %{pkg_version}
Release:        1%{?dist}
Summary:        Pingora-based HTTP, TCP and RTMP proxy runtime
License:        Apache-2.0
URL:            https://github.com/brauliobo/oxiroute
ExclusiveArch:  x86_64 aarch64
BuildRequires:  systemd-rpm-macros
Requires:       ca-certificates
Requires(pre):  systemd
%{?systemd_requires}
Suggests:       certbot

%description
OxiRoute routes HTTP, TCP, UDP and RTMP services from one typed configuration.
It ships a management CLI (oxiroute, alias oxr), a browser dashboard and a
systemd service.

The package enables and starts nothing: edit /etc/oxiroute/oxiroute.kdl, then
run "systemctl enable --now oxiroute.service".

%prep
cp -p %{stage}/payload/LICENSE %{stage}/payload/BUILD-INFO.json %{stage}/docs/*.md .

%build

%install
install -Dm755 %{stage}/payload/bin/oxiroute %{buildroot}%{_bindir}/oxiroute
ln -s oxiroute %{buildroot}%{_bindir}/oxr
install -Dm644 -t %{buildroot}%{_datadir}/bash-completion/completions %{stage}/payload/share/bash-completion/completions/*
install -Dm644 -t %{buildroot}%{_datadir}/zsh/site-functions %{stage}/payload/share/zsh/site-functions/*
install -Dm644 -t %{buildroot}%{_datadir}/fish/vendor_completions.d %{stage}/payload/share/fish/vendor_completions.d/*
install -Dm644 -t %{buildroot}%{_mandir}/man1 %{stage}/payload/share/man/man1/*
install -Dm755 %{stage}/payload/lib/oxiroute/oxiroute-worker-launcher %{buildroot}%{_prefix}/lib/oxiroute/oxiroute-worker-launcher
install -Dm644 %{stage}/layout/oxiroute.service %{buildroot}%{_unitdir}/oxiroute.service
install -Dm644 %{stage}/layout/oxiroute.sysusers %{buildroot}%{_sysusersdir}/oxiroute.conf
install -Dm644 %{stage}/layout/oxiroute.tmpfiles %{buildroot}%{_tmpfilesdir}/oxiroute.conf
install -Dm644 %{stage}/layout/oxiroute.modules %{buildroot}%{_prefix}/lib/modules-load.d/oxiroute.conf
install -Dm644 -t %{buildroot}%{_sysconfdir}/oxiroute %{stage}/layout/oxiroute.env %{stage}/layout/oxiroute.kdl %{stage}/layout/oxiroute.lua

%pre
%sysusers_create_package oxiroute %{stage}/layout/oxiroute.sysusers

%post
%tmpfiles_create_package oxiroute %{stage}/layout/oxiroute.tmpfiles
%systemd_post oxiroute.service

%preun
%systemd_preun oxiroute.service

%postun
%systemd_postun_with_restart oxiroute.service

%files
%license LICENSE
%doc MANAGEMENT_CLI.md INSTALL.md BUILD-INFO.json
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
%{_prefix}/lib/oxiroute/oxiroute-worker-launcher
%{_unitdir}/oxiroute.service
%{_sysusersdir}/oxiroute.conf
%{_tmpfilesdir}/oxiroute.conf
%{_prefix}/lib/modules-load.d/oxiroute.conf
%dir %{_sysconfdir}/oxiroute
%config(noreplace) %{_sysconfdir}/oxiroute/oxiroute.env
%config(noreplace) %{_sysconfdir}/oxiroute/oxiroute.kdl
%config(noreplace) %{_sysconfdir}/oxiroute/oxiroute.lua
