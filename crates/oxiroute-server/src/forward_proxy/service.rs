#[allow(clippy::wildcard_imports)]
use super::*;

impl ForwardHttp1ServicePlan {
    #[allow(clippy::too_many_lines)]
    pub(crate) fn acquire(
        blueprint: ForwardServiceBlueprint,
        cache: Option<Arc<HttpCachePlan>>,
    ) -> Result<Self, ForwardPlanError> {
        let auth = match &blueprint.auth {
            Some(ForwardAuthBlueprint::Bearer { token_file_path }) => Some(
                SecureBearerToken::load(token_file_path)
                    .map(ForwardAuthPlan::Bearer)
                    .map_err(|_| ForwardPlanError::Authentication)?,
            ),
            Some(ForwardAuthBlueprint::Basic {
                htpasswd_file_path,
                realm,
                credential_ttl,
                username_case_sensitive,
            }) => {
                let access = BasicHtpasswdAccess::load_with_username_case(
                    htpasswd_file_path,
                    realm,
                    *username_case_sensitive,
                )
                .map_err(|_| ForwardPlanError::Authentication)?;
                Some(ForwardAuthPlan::Basic(Box::new(ForwardBasicAuth::new(
                    access,
                    htpasswd_file_path.clone(),
                    realm.clone(),
                    *credential_ttl,
                    *username_case_sensitive,
                )?)))
            }
            None => None,
        };
        let resolver = acquire_resolver(&blueprint.resolver)?;
        let mut connector =
            SslConnector::builder(SslMethod::tls_client()).map_err(|_| ForwardPlanError::Tls)?;
        connector.set_verify(SslVerifyMode::PEER);
        let max_connections = blueprint.max_connections;
        let max_header_bytes = blueprint.max_header_bytes;
        let max_request_body_bytes = blueprint.max_request_body_bytes;
        let h3_upstream = blueprint
            .h3_upstream_connections
            .map(|max_connections| {
                H3UpstreamPlan::for_forward(max_connections)
                    .map(Arc::new)
                    .map_err(|_| ForwardPlanError::Tls)
            })
            .transpose()?;
        let resolver_addresses = blueprint.resolver_addresses;
        let resolver_queries = blueprint.resolver_queries;
        let local_addresses = local_ip_address::list_afinet_netifas()
            .map_err(|_| ForwardPlanError::Resolver)?
            .into_iter()
            .map(|(_, address)| address)
            .collect();
        let mut http_server_options = HttpServerOptions::default();
        http_server_options.h2c = true;

        Ok(Self {
            access_policy: blueprint.access_policy,
            allow_absolute_form: blueprint.allow_absolute_form,
            audit_mode: blueprint.audit_mode,
            auth,
            challenge: blueprint.challenge,
            connect_enabled: blueprint.connect_enabled,
            connect_ports: blueprint.connect_ports,
            connect_udp_enabled: blueprint.connect_udp_enabled,
            connect_udp_ports: blueprint.connect_udp_ports,
            connect_timeout: blueprint.connect_timeout,
            destination_policy: blueprint.destination_policy,
            peer_direct_fallback: blueprint.peer_direct_fallback,
            peer_max_retries: blueprint.peer_max_retries,
            peers: blueprint.peers,
            header_policy: blueprint.header_policy,
            http_server_options,
            idle_timeout: blueprint.idle_timeout,
            lifetime_timeout: blueprint.lifetime_timeout,
            local_addresses: Arc::new(local_addresses),
            max_header_bytes,
            max_request_body_bytes,
            name: blueprint.name,
            resolver,
            resolver_addresses,
            resolver_revalidate_on_connect: blueprint.resolver_revalidate_on_connect,
            resolver_queries: Arc::new(Semaphore::new(resolver_queries)),
            service_connections: Arc::new(Semaphore::new(max_connections)),
            tls_connector: Arc::new(connector.build()),
            access_metrics: Arc::new(ForwardAccessMetrics::default()),
            cache,
            h3_upstream,
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn max_header_bytes(&self) -> usize {
        self.max_header_bytes
    }

    #[must_use]
    pub const fn idle_timeout(&self) -> Duration {
        self.idle_timeout
    }

    #[must_use]
    pub const fn lifetime_timeout(&self) -> Duration {
        self.lifetime_timeout
    }

    pub fn begin_connection(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.service_connections)
            .try_acquire_owned()
            .ok()
    }

    pub(super) fn h3_upstream(&self) -> Option<&H3UpstreamPlan> {
        self.h3_upstream.as_deref()
    }

    #[must_use]
    pub fn forward_access_metrics(&self) -> ForwardAccessMetricsSnapshot {
        self.access_metrics.snapshot()
    }
}

impl ForwardHttp1ServicePlan {
    pub(super) fn rejection(&self, failure: RequestFailure) -> Response<ForwardProxyBody> {
        let mut response = response(failure.status(), Bytes::new());
        if failure == RequestFailure::Authentication
            && let Some(challenge) = &self.challenge
        {
            response
                .headers_mut()
                .insert(header::PROXY_AUTHENTICATE, challenge.clone());
        }
        response
    }

    pub(super) async fn reject_h2(&self, session: &mut ServerSession, failure: RequestFailure) {
        let Ok(mut response_header) = pingora::http::ResponseHeader::build(failure.status(), None)
        else {
            session.shutdown().await;
            return;
        };
        if failure == RequestFailure::Authentication
            && let Some(challenge) = &self.challenge
        {
            let _ = response_header.insert_header(header::PROXY_AUTHENTICATE, challenge.clone());
        }
        if session
            .write_response_header(Box::new(response_header))
            .await
            .is_err()
        {
            return;
        }
        let _ = session.finish_body().await;
    }
}
