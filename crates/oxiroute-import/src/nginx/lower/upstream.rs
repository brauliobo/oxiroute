use crate::{E_SEMANTICS_NOT_REPRESENTABLE, E_UNSUPPORTED_FEATURE};
use oxiroute_config::{UpstreamEndpoint, UpstreamTls};

use crate::nginx::{
    EffectiveHttp, EffectiveProxyPass, EffectiveUpstream, ProxyPassScheme, StaticEndpoint,
    UpstreamReference,
};

use super::{LowerIssue, Lowerer, provenance::issue};

pub(super) fn canonical_endpoint(endpoint: &StaticEndpoint) -> UpstreamEndpoint {
    match endpoint {
        StaticEndpoint::Socket { address } => UpstreamEndpoint::Socket { address: *address },
        StaticEndpoint::Dns { host, port } => UpstreamEndpoint::Dns {
            host: host.clone(),
            port: *port,
        },
        StaticEndpoint::Unix { path } => UpstreamEndpoint::Unix { path: path.clone() },
    }
}

impl Lowerer {
    pub(super) fn validate_proxy_origin(
        &self,
        http: &EffectiveHttp,
        proxy: &EffectiveProxyPass,
        downstream_tls: bool,
    ) -> Result<Option<UpstreamTls>, Vec<LowerIssue>> {
        let tls = self.proxy_origin_tls(proxy, downstream_tls)?;
        match proxy.upstream {
            UpstreamReference::Direct => Self::validate_direct_origin(proxy),
            UpstreamReference::Resolved(occurrence) => {
                let upstream = http
                    .upstreams
                    .iter()
                    .find(|upstream| upstream.origin.occurrence == occurrence)
                    .expect("resolved upstream occurrence is retained");
                self.validate_upstream_origin(upstream, proxy)
            }
            UpstreamReference::Unresolved | UpstreamReference::Variable => Err(vec![issue(
                &proxy.origin,
                E_UNSUPPORTED_FEATURE,
                "dynamic or unresolved proxy origin cannot be lowered",
            )]),
        }?;
        Ok(tls)
    }

    fn proxy_origin_tls(
        &self,
        proxy: &EffectiveProxyPass,
        downstream_tls: bool,
    ) -> Result<Option<UpstreamTls>, Vec<LowerIssue>> {
        let secure = match proxy.scheme {
            ProxyPassScheme::Http => false,
            ProxyPassScheme::Https => true,
            ProxyPassScheme::Downstream => downstream_tls,
            ProxyPassScheme::Unsupported(_) => {
                return Err(vec![issue(
                    &proxy.origin,
                    E_UNSUPPORTED_FEATURE,
                    "unsupported proxy origin scheme cannot be lowered",
                )]);
            }
        };
        if !secure {
            return Ok(None);
        }
        let authority = proxy
            .authority
            .iter()
            .map(u8::to_ascii_lowercase)
            .collect::<Vec<_>>();
        if let Some(tls) = self.upstream_tls_overlays.get(&authority) {
            self.used_upstream_tls_overlays
                .borrow_mut()
                .insert(authority);
            return Ok(Some(tls.clone()));
        }
        if let Some(StaticEndpoint::Dns { host, .. }) = &proxy.direct_endpoint {
            return Ok(Some(UpstreamTls {
                server_name: host.clone(),
                ca_certificate_path: None,
            }));
        }
        Err(vec![issue(
            &proxy.origin,
            E_SEMANTICS_NOT_REPRESENTABLE,
            "HTTPS proxy origin with an IP or named-upstream authority requires an explicit verified DNS SNI/trust overlay",
        )])
    }

    fn validate_direct_origin(proxy: &EffectiveProxyPass) -> Result<(), Vec<LowerIssue>> {
        if proxy
            .direct_endpoint
            .as_ref()
            .map(canonical_endpoint)
            .is_none()
        {
            return Err(vec![issue(
                &proxy.origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "direct proxy origin is not a canonical socket, DNS, or Unix endpoint",
            )]);
        }
        Ok(())
    }

    fn validate_upstream_origin(
        &self,
        upstream: &EffectiveUpstream,
        proxy: &EffectiveProxyPass,
    ) -> Result<(), Vec<LowerIssue>> {
        let mut issues = self.blocking_subtree_issues(upstream.origin.occurrence);
        let mut endpoint_count = 0;
        for server in &upstream.servers {
            if server.endpoint.as_ref().map(canonical_endpoint).is_some() {
                endpoint_count += 1;
            } else {
                issues.push(issue(
                    &server.origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "upstream server is not a canonical socket, DNS, or Unix endpoint",
                ));
            }
        }
        if endpoint_count == 0 {
            issues.push(issue(
                &proxy.origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "referenced upstream has no canonical endpoints",
            ));
        }
        if issues.is_empty() {
            Ok(())
        } else {
            Err(issues)
        }
    }
}
