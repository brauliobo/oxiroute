use std::collections::HashSet;

use crate::apache::{EffectiveListen, EffectiveVirtualHost};
use crate::{E_SEMANTICS_NOT_REPRESENTABLE, ProvenanceRole};
use oxiroute_config::{
    DownstreamTimeoutPolicy, HttpRoute, HttpRouteAction, HttpService, Listener, ListenerBind,
    Protocol,
};

use super::{ApacheProvenance, BlockedVirtualHost, Lowerer};

impl Lowerer {
    #[allow(clippy::too_many_lines)]
    pub(super) fn lower_listener(
        &mut self,
        listen: &EffectiveListen,
        virtual_hosts: &[EffectiveVirtualHost],
    ) {
        for virtual_host in virtual_hosts
            .iter()
            .filter(|virtual_host| !virtual_host.blocked.is_empty())
        {
            self.record_blocked_virtual_host(virtual_host);
        }
        let usable = virtual_hosts
            .iter()
            .filter(|virtual_host| virtual_host.blocked.is_empty())
            .cloned()
            .collect::<Vec<_>>();
        if usable.is_empty() {
            return;
        }

        let tls_modes = usable
            .iter()
            .map(|virtual_host| virtual_host.tls.engine_on)
            .collect::<HashSet<_>>();
        if tls_modes.len() > 1 {
            self.block_origin(
                &usable[0].origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "Apache virtual hosts sharing one listener disagree about TLS termination",
            );
            for virtual_host in &usable {
                self.record_blocked_virtual_host_with_code(
                    virtual_host,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                );
            }
            return;
        }

        let mut routes = Vec::new();
        let mut route_origins = Vec::new();
        let mut listener_origins = vec![self.origin(&listen.origin, ProvenanceRole::Declaration)];
        for (vhost_index, virtual_host) in usable.iter().enumerate() {
            let Some(lowered) = self.lower_virtual_host(virtual_host, vhost_index == 0) else {
                self.record_blocked_virtual_host(virtual_host);
                continue;
            };
            listener_origins.extend(lowered.origins.clone());
            routes.extend(lowered.routes);
            route_origins.extend(lowered.route_origins);
        }
        if routes.is_empty() {
            return;
        }

        let listener_index = self.draft.listeners.len();
        let service_name = format!("apache-http-{}", listener_index + 1);
        let tls_profile = tls_modes
            .contains(&true)
            .then(|| self.lower_tls(&usable, listener_index));
        if let Some((certificates, profile, tls_origins)) = tls_profile {
            self.commit_certificates(certificates, &tls_origins);
            let profile_index = self.draft.tls_profiles.len();
            let profile_name = profile.name.clone();
            self.draft.tls_profiles.push(profile);
            let profile_path = format!("/tls_profiles/{profile_index}");
            for suffix in [
                "",
                "/name",
                "/certificates",
                "/default_certificate",
                "/min_version",
                "/alpn",
            ] {
                self.record(format!("{profile_path}{suffix}"), tls_origins.clone());
            }
            let listener = Listener {
                name: format!("apache-listener-{}", listener_index + 1),
                bind: ListenerBind::Socket {
                    address: listen.address,
                    ipv6_only: false,
                },
                protocol: Protocol::Http,
                service: Some(service_name.clone()),
                tls_profile: Some(profile_name),
                proxy_protocol: None,
                max_connections: None,
                downstream_timeouts: DownstreamTimeoutPolicy::default(),
                h2_initial_stream_window: None,
                h2_initial_connection_window: None,
            };
            self.commit_listener_and_service(
                listener,
                service_name,
                routes,
                route_origins,
                &listener_origins,
            );
        } else {
            let listener = Listener {
                name: format!("apache-listener-{}", listener_index + 1),
                bind: ListenerBind::Socket {
                    address: listen.address,
                    ipv6_only: false,
                },
                protocol: Protocol::Http,
                service: Some(service_name.clone()),
                tls_profile: None,
                proxy_protocol: None,
                max_connections: None,
                downstream_timeouts: DownstreamTimeoutPolicy::default(),
                h2_initial_stream_window: None,
                h2_initial_connection_window: None,
            };
            self.commit_listener_and_service(
                listener,
                service_name,
                routes,
                route_origins,
                &listener_origins,
            );
        }
    }

    pub(super) fn commit_listener_and_service(
        &mut self,
        listener: Listener,
        service_name: String,
        routes: Vec<HttpRoute>,
        route_origins: Vec<Vec<ApacheProvenance>>,
        listener_origins: &[ApacheProvenance],
    ) {
        let listener_index = self.draft.listeners.len();
        self.draft.listeners.push(listener);
        let listener_path = format!("/listeners/{listener_index}");
        for suffix in [
            "",
            "/name",
            "/bind",
            "/bind/type",
            "/bind/address",
            "/protocol",
            "/service",
        ] {
            self.record(
                format!("{listener_path}{suffix}"),
                listener_origins.to_vec(),
            );
        }
        let service_index = self.draft.http_services.len();
        self.draft.http_services.push(HttpService {
            drain_deadline_ms: None,
            name: service_name,
            routes,
            automatic_response_headers: false,
            upstream_io_timeout_ms: 30_000,
            max_request_body_bytes: None,
            gzip: None,
            access_log: None,
        });
        let service_path = format!("/http_services/{service_index}");
        self.record(service_path.clone(), listener_origins.to_vec());
        self.record(
            format!("{service_path}/automatic_response_headers"),
            listener_origins.to_vec(),
        );
        self.record(
            format!("{service_path}/upstream_io_timeout_ms"),
            listener_origins.to_vec(),
        );
        self.record(
            format!("{service_path}/max_request_body_bytes"),
            listener_origins.to_vec(),
        );
        for (route_index, origins) in route_origins.into_iter().enumerate() {
            let route_path = format!("{service_path}/routes/{route_index}");
            let route = self.draft.http_services[service_index].routes[route_index].clone();
            for suffix in [
                "",
                "/path",
                "/path/kind",
                "/path/value",
                "/methods",
                "/action",
                "/action/type",
            ] {
                self.record(format!("{route_path}{suffix}"), origins.clone());
            }
            if route.host.is_some() {
                for suffix in ["/host", "/host/kind", "/host/value"] {
                    self.record(format!("{route_path}{suffix}"), origins.clone());
                }
            }
            if let HttpRouteAction::Proxy { .. } = &route.action {
                for suffix in [
                    "/action/upstream_pool",
                    "/action/policy",
                    "/action/policy/upstream_host",
                    "/action/policy/upstream_host/type",
                    "/action/policy/request_headers",
                    "/action/policy/response_headers",
                    "/action/policy/retry",
                ] {
                    self.record(format!("{route_path}{suffix}"), origins.clone());
                }
            }
        }
    }

    pub(super) fn record_blocked_virtual_host(&mut self, virtual_host: &EffectiveVirtualHost) {
        if virtual_host.blocked.is_empty() {
            return;
        }
        self.blocked_virtual_hosts.push(BlockedVirtualHost {
            address: virtual_host.address,
            origin: self.origin(&virtual_host.origin, ProvenanceRole::Declaration),
            diagnostic_codes: virtual_host.blocked.clone(),
        });
    }

    pub(super) fn record_blocked_virtual_host_with_code(
        &mut self,
        virtual_host: &EffectiveVirtualHost,
        code: crate::DiagnosticCode,
    ) {
        let mut virtual_host = virtual_host.clone();
        virtual_host.blocked.push(code);
        self.record_blocked_virtual_host(&virtual_host);
    }
}

pub(super) fn socket_addresses_overlap(
    first: std::net::SocketAddr,
    second: std::net::SocketAddr,
) -> bool {
    first.port() == second.port()
        && (first.ip() == second.ip()
            || first.ip().is_unspecified()
            || second.ip().is_unspecified())
}
