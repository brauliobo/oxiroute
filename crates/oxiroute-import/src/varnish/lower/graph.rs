#[allow(clippy::wildcard_imports)]
use super::*;

impl Lowerer<'_> {
    #[allow(clippy::needless_borrow, clippy::too_many_lines)]
    pub(super) fn lower_builtin_graph(&mut self) {
        let recv = self.phase(SubroutineKind::Recv);
        let mut selected = None;
        let mut cache_enabled = true;
        let mut request_headers = Vec::new();
        let mut terminal = None;
        for (index, statement) in recv.iter().enumerate() {
            match &statement.classification {
                StatementClassification::BackendSelection(reference) => {
                    if selected.replace(reference.clone()).is_some() {
                        self.block_statement(
                            LoweringBlocker::SemanticMismatch,
                            E_VCL_SEMANTIC_MISMATCH,
                            &statement,
                            "backend selection is assigned more than once",
                        );
                    }
                }
                StatementClassification::HeaderMutation(mutation)
                    if matches!(
                        mutation.scope,
                        HeaderScope::Request | HeaderScope::BackendRequest
                    ) =>
                {
                    if let Some(mutation) = request_mutation(mutation) {
                        request_headers.push(mutation);
                    } else {
                        self.block_statement(
                            LoweringBlocker::SemanticMismatch,
                            E_VCL_SEMANTIC_MISMATCH,
                            &statement,
                            "request header mutation is not a static canonical set/remove operation",
                        );
                    }
                }
                StatementClassification::CacheDecision(action) => {
                    if terminal.is_some() || index + 1 != recv.len() {
                        self.block_statement(
                            LoweringBlocker::SemanticMismatch,
                            E_VCL_SEMANTIC_MISMATCH,
                            &statement,
                            "terminal VCL flow action is not the final statement in its phase",
                        );
                    }
                    terminal = Some(*action);
                    match action {
                        FlowAction::Hash | FlowAction::Lookup => cache_enabled = true,
                        FlowAction::Pass => cache_enabled = false,
                        FlowAction::Pipe => self.block_statement(
                            LoweringBlocker::UnsupportedBehavior,
                            E_UNSUPPORTED_FEATURE,
                            &statement,
                            "pipe mode has no equivalent canonical HTTP service action",
                        ),
                        _ => self.block_statement(
                            LoweringBlocker::SemanticMismatch,
                            E_VCL_SEMANTIC_MISMATCH,
                            &statement,
                            "VCL flow action is not representable by the canonical HTTP service",
                        ),
                    }
                }
                _ => self.block_statement(
                    LoweringBlocker::UnsupportedBehavior,
                    E_VCL_LOWERING_BLOCKED,
                    &statement,
                    "VCL receive-phase statement is outside the exact linear graph",
                ),
            }
        }
        if terminal.is_none() {
            cache_enabled = true;
        }

        self.validate_hash_phase();
        let mut response_headers = Vec::new();
        let mut cache_ttl = None;
        let mut cache_grace = None;
        let mut cache_keep = None;
        let mut explicit_ttl = false;
        for statement in self
            .phase(SubroutineKind::BackendResponse)
            .into_iter()
            .chain(self.phase(SubroutineKind::Deliver))
        {
            match &statement.classification {
                StatementClassification::CacheLifetime(lifetime) => {
                    let value = duration_ms(&lifetime.value);
                    if lifetime.operator != super::super::AssignmentOperator::Set || value.is_none() {
                        self.block_statement(
                            LoweringBlocker::SemanticMismatch,
                            E_VCL_SEMANTIC_MISMATCH,
                            &statement,
                            "cache lifetime must be assigned one finite whole-millisecond literal",
                        );
                        continue;
                    }
                    let value = value.expect("checked cache lifetime");
                    let slot = match lifetime.field {
                        CacheLifetimeField::Ttl => {
                            explicit_ttl = true;
                            &mut cache_ttl
                        }
                        CacheLifetimeField::Grace => &mut cache_grace,
                        CacheLifetimeField::Keep => &mut cache_keep,
                    };
                    if slot.replace(value).is_some() {
                        self.block_statement(
                            LoweringBlocker::SemanticMismatch,
                            E_VCL_SEMANTIC_MISMATCH,
                            &statement,
                            "cache lifetime field is assigned more than once",
                        );
                    }
                }
                StatementClassification::CacheFlag(flag) => match flag {
                    CacheFlag::Uncacheable(value) => match boolean(value) {
                        Some(false) => {}
                        Some(true) => self.block_statement(
                            LoweringBlocker::SemanticMismatch,
                            E_VCL_SEMANTIC_MISMATCH,
                            &statement,
                            "uncacheable backend responses do not have an exact canonical response policy",
                        ),
                        None => self.block_statement(
                            LoweringBlocker::SemanticMismatch,
                            E_VCL_SEMANTIC_MISMATCH,
                            &statement,
                            "uncacheable must be assigned a static boolean",
                        ),
                    },
                    CacheFlag::HitForPass { .. } | CacheFlag::BackgroundFetch(_) => self
                        .block_statement(
                            LoweringBlocker::UnsupportedBehavior,
                            E_UNSUPPORTED_FEATURE,
                            &statement,
                            "hit-for-pass or background fetch is outside the exact cache subset",
                        ),
                },
                StatementClassification::Feature(FeatureBehavior::Compression { operation, enabled }) => {
                    match (operation, boolean(enabled)) {
                        (CompressionOperation::Gzip | CompressionOperation::Gunzip, Some(false)) => {}
                        (CompressionOperation::Gzip, Some(true)) => self.block_statement(
                            LoweringBlocker::SemanticMismatch,
                            E_VCL_SEMANTIC_MISMATCH,
                            &statement,
                            "Varnish gzip defaults do not exactly identify the canonical gzip policy",
                        ),
                        (CompressionOperation::Gunzip, Some(true)) => self.block_statement(
                            LoweringBlocker::UnsupportedBehavior,
                            E_UNSUPPORTED_FEATURE,
                            &statement,
                            "backend gunzip has no canonical response equivalent",
                        ),
                        (_, None) => self.block_statement(
                            LoweringBlocker::SemanticMismatch,
                            E_VCL_SEMANTIC_MISMATCH,
                            &statement,
                            "compression behavior must be assigned a static boolean",
                        ),
                    }
                }
                StatementClassification::HeaderMutation(mutation)
                    if matches!(
                        mutation.scope,
                        HeaderScope::BackendResponse | HeaderScope::Response | HeaderScope::Object
                    ) =>
                {
                    if let Some(mutation) = response_mutation(mutation) {
                        response_headers.push(mutation);
                    } else {
                        self.block_statement(
                            LoweringBlocker::SemanticMismatch,
                            E_VCL_SEMANTIC_MISMATCH,
                            &statement,
                            "response header mutation is not a static canonical set/add/remove operation",
                        );
                    }
                }
                StatementClassification::CacheDecision(FlowAction::Deliver) => {}
                _ => self.block_statement(
                    LoweringBlocker::UnsupportedBehavior,
                    E_VCL_LOWERING_BLOCKED,
                    &statement,
                    "VCL backend-response or deliver behavior is outside the exact cache graph",
                ),
            }
        }
        let backend_fetch_headers = self
            .phase(SubroutineKind::BackendFetch)
            .into_iter()
            .filter_map(|statement| match &statement.classification {
                StatementClassification::HeaderMutation(mutation)
                    if matches!(mutation.scope, HeaderScope::BackendRequest | HeaderScope::Request) =>
                {
                    request_mutation(mutation).or_else(|| {
                        self.block_statement(
                            LoweringBlocker::SemanticMismatch,
                            E_VCL_SEMANTIC_MISMATCH,
                            &statement,
                            "backend request header mutation is not a static canonical set/remove operation",
                        );
                        None
                    })
                }
                _ => {
                    self.block_statement(
                        LoweringBlocker::UnsupportedBehavior,
                        E_VCL_LOWERING_BLOCKED,
                        &statement,
                        "VCL backend-fetch behavior is outside the exact request graph",
                    );
                    None
                }
            })
            .collect::<Vec<_>>();
        request_headers.extend(backend_fetch_headers);

        if cache_enabled && !request_headers.is_empty() {
            self.block(
                LoweringBlocker::SemanticMismatch,
                E_VCL_SEMANTIC_MISMATCH,
                DiagnosticStage::Lower,
                "static request header mutation with cache changes backend-only semantics that the active cache plan rejects",
                self.root_span(),
            );
        }

        let selected = selected
            .as_ref()
            .and_then(|reference| self.selected_pool(reference));
        let Some((pool_name, timing, backend_origin)) = selected else {
            self.block(
                LoweringBlocker::SemanticMismatch,
                E_VCL_SEMANTIC_MISMATCH,
                DiagnosticStage::Lower,
                "VCL graph does not select one static backend or director",
                self.root_span(),
            );
            return;
        };
        let Some(invocation_policy) = self.invocation_policy.clone() else {
            return;
        };
        let cache = if cache_enabled {
            let Some(store) = invocation_policy.storage.as_ref() else {
                self.block(
                    LoweringBlocker::SemanticMismatch,
                    E_VCL_SEMANTIC_MISMATCH,
                    DiagnosticStage::Lower,
                    "VCL cache flow requires a memory or disk varnishd storage backend",
                    self.root_span(),
                );
                return;
            };
            let store_name = store.name().to_owned();
            let ttl = cache_ttl.unwrap_or(invocation_policy.default_ttl_ms);
            let grace = cache_grace.unwrap_or(invocation_policy.default_grace_ms);
            let keep = cache_keep.unwrap_or(invocation_policy.default_keep_ms);
            if grace > keep {
                self.block(
                    LoweringBlocker::SemanticMismatch,
                    E_VCL_SEMANTIC_MISMATCH,
                    DiagnosticStage::Lower,
                    "VCL grace exceeds keep and cannot be represented by the canonical cache timeline",
                    self.root_span(),
                );
            }
            Some(Box::new(HttpCachePolicy {
                store: store_name,
                methods: vec!["GET".into(), "HEAD".into()],
                key_components: vec![
                    CacheKeyComponent::Scheme,
                    CacheKeyComponent::NormalizedHost,
                    CacheKeyComponent::PathAndQuery,
                ],
                use_origin_cache_control: !explicit_ttl,
                default_ttl_ms: ttl,
                status_ttls: Vec::new(),
                grace_ms: grace,
                keep_ms: keep,
                revalidate: true,
                collapsed_forwarding: true,
                stale_on: Vec::new(),
                bypass_request: Vec::new(),
                no_store_request: Vec::new(),
                no_store_response: Vec::new(),
                set_cookie_policy: oxiroute_config::CacheSetCookiePolicy::default(),
                authorization_policy: oxiroute_config::CacheAuthorizationPolicy::default(),
                vary_policy: oxiroute_config::CacheVaryPolicy::default(),
                surrogate_tags: None,
                purge_authorization: None,
            }))
        } else {
            None
        };

        let pool_origin =
            backend_origin.unwrap_or_else(|| self.root_origin.clone().expect("root origin"));
        let service_name = "varnish-http".to_owned();
        let route = HttpRoute {
            host: None,
            path: oxiroute_config::HttpPathSelector::SegmentPrefix { value: "/".into() },
            methods: Vec::new(),
            access_policy: None,
            policy: HttpRoutePolicy {
                max_request_body_bytes: None,
                connect_timeout_ms: timing.connect_ms,
                read_timeout_ms: timing.read_ms,
                write_timeout_ms: timing.write_ms,
                request_buffering: false,
                response_buffering: false,
            },
            action: HttpRouteAction::Proxy {
                upstream_pool: pool_name,
                policy: HttpProxyPolicy {
                    upstream_host: HttpUpstreamHost::PreserveIncoming,
                    upstream_path_rewrite: None,
                    request_headers,
                    response_headers,
                    response_cookie_path_rewrites: Vec::new(),
                    response_cookie_attributes: Vec::new(),
                    retry: oxiroute_config::HttpRetryPolicy::default(),
                    timeouts: oxiroute_config::HttpProxyTimeouts::default(),
                    rescue: None,
                    select: None,
                    cache,
                },
            },
        };
        self.draft.http_services.push(HttpService {
            name: service_name.clone(),
            routes: vec![route],
            automatic_response_headers: false,
            upstream_io_timeout_ms: timing.read_ms,
            max_request_body_bytes: None,
            gzip: None,
            access_log: None,
        });
        self.record("/http_services/0".into(), vec![pool_origin.clone()]);
        for suffix in [
            "/name",
            "/automatic_response_headers",
            "/upstream_io_timeout_ms",
            "/max_request_body_bytes",
            "/routes/0",
            "/routes/0/path",
            "/routes/0/action",
            "/routes/0/action/upstream_pool",
            "/routes/0/action/policy",
        ] {
            self.record(
                format!("/http_services/0{suffix}"),
                vec![pool_origin.clone()],
            );
        }
        if cache_enabled {
            self.record(
                "/http_services/0/routes/0/action/policy/cache".into(),
                self.phase_origin(SubroutineKind::BackendResponse),
            );
        }
        for (index, address) in invocation_policy.listeners.iter().copied().enumerate() {
            self.draft.listeners.push(Listener {
                name: format!("varnish-listener-{}", index + 1),
                bind: ListenerBind::Socket { address },
                protocol: Protocol::Http,
                service: Some(service_name.clone()),
                tls_profile: None,
                proxy_protocol: None,
                max_connections: None,
                downstream_timeouts: DownstreamTimeoutPolicy::default(),
            });
            let path = format!("/listeners/{index}");
            self.record(path.clone(), vec![pool_origin.clone()]);
            self.record(format!("{path}/bind"), vec![pool_origin.clone()]);
            self.record(format!("{path}/service"), vec![pool_origin.clone()]);
            self.record(format!("{path}/protocol"), vec![pool_origin.clone()]);
        }
        if let Some(store) = invocation_policy.storage.as_ref() {
            let index = self.draft.cache_stores.len();
            self.draft.cache_stores.push(store.clone());
            let path = format!("/cache_stores/{index}");
            self.record(path.clone(), self.root_origin.clone().into_iter().collect());
            self.record(
                format!("{path}/name"),
                self.root_origin.clone().into_iter().collect(),
            );
        }
    }

    fn validate_hash_phase(&mut self) {
        let statements = self.phase(SubroutineKind::Hash);
        if statements.is_empty() {
            return;
        }
        let mut keys = Vec::new();
        let mut lookup = false;
        for statement in &statements {
            match &statement.classification {
                StatementClassification::Hash(expression) => keys.push(expression),
                StatementClassification::CacheDecision(FlowAction::Lookup) => lookup = true,
                _ => self.block_statement(
                    LoweringBlocker::SemanticMismatch,
                    E_VCL_SEMANTIC_MISMATCH,
                    statement,
                    "VCL hash phase is not the canonical URL/Host hash graph",
                ),
            }
        }
        let exact = keys.len() == 2
            && keys
                .iter()
                .any(|expression| expression_name(expression) == Some(b"req.url"))
            && keys
                .iter()
                .any(|expression| expression_name(expression).is_some_and(is_host_name));
        if !exact || !lookup {
            let span = statements
                .first()
                .map(|statement| statement.provenance.span);
            self.block(
                LoweringBlocker::SemanticMismatch,
                E_VCL_SEMANTIC_MISMATCH,
                DiagnosticStage::Lower,
                "VCL hash phase must contain exactly req.url, req.http.Host, and return(lookup)",
                span,
            );
        }
    }

    fn selected_pool(
        &self,
        reference: &BackendReference,
    ) -> Option<(String, RouteTiming, Option<Provenance>)> {
        match reference {
            BackendReference::Backend { declaration, .. } => {
                self.backend_infos.get(declaration).map(|info| {
                    (
                        info.pool.name.clone(),
                        info.timing,
                        self.backend_origin(*declaration),
                    )
                })
            }
            BackendReference::Director {
                declaration,
                modern: false,
                ..
            } => self.director_infos.get(declaration).map(|info| {
                (
                    info.pool.name.clone(),
                    info.timing,
                    self.director_origin(*declaration),
                )
            }),
            BackendReference::Director { modern: true, .. }
            | BackendReference::None
            | BackendReference::Unresolved { .. }
            | BackendReference::Dynamic(_) => None,
        }
    }
}
