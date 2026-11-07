#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn revision_domains_have_exact_lowercase_sha256_wire_behavior() {
    let authored = AuthoredRevision::from_bytes(b"authored");
    let effective = EffectiveRevision::from_bytes(b"effective");

    for revision in [authored.as_str(), effective.as_str()] {
        assert_eq!(revision.len(), 64);
        assert!(
            revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
    }
    assert_eq!(authored.to_string(), authored.as_str());
    assert_eq!(effective.to_string(), effective.as_str());
    assert_eq!(
        serde_json::to_string(&authored).unwrap(),
        format!("\"{authored}\"")
    );
    assert_eq!(
        serde_json::to_string(&effective).unwrap(),
        format!("\"{effective}\"")
    );
    assert_eq!(authored.as_str().parse::<AuthoredRevision>(), Ok(authored));
    assert_eq!(
        effective.as_str().parse::<EffectiveRevision>(),
        Ok(effective)
    );
    assert!("A".repeat(64).parse::<AuthoredRevision>().is_err());
    assert!("A".repeat(64).parse::<EffectiveRevision>().is_err());
    assert!("g".repeat(64).parse::<AuthoredRevision>().is_err());
    assert!("0".repeat(63).parse::<EffectiveRevision>().is_err());
}

#[test]
fn loaded_document_retains_validated_proof_and_both_revision_domains() {
    let (_temp, path, coordinator) = fixture(&normalizable_config());
    let authored_bytes = fs::read(path).unwrap();

    let document = loaded(coordinator.load());

    assert_eq!(
        document.authored_revision,
        AuthoredRevision::from_bytes(&authored_bytes)
    );
    assert_eq!(
        document.effective_revision,
        EffectiveRevision::from_bytes(
            render_config(ConfigFormat::Kdl, &document.validated_config)
                .unwrap()
                .as_bytes()
        )
    );
    assert_eq!(
        document.validated_config.as_draft().http_services[0].routes[0]
            .host
            .as_ref(),
        Some(&HttpHostSelector::NormalizedHost {
            value: "api.example.test".into(),
        })
    );
}

#[test]
fn prepared_candidate_bytes_are_consumed_by_save() {
    let (_temp, path, coordinator) = fixture(&minimal_config());
    let expected = loaded(coordinator.load()).authored_revision;
    let ConfigValidationOutcome::Valid(candidate) = coordinator.prepare(normalizable_config())
    else {
        panic!("candidate preparation failed")
    };
    let expected_preview = candidate.config_preview().to_owned();
    let expected_effective = candidate.effective_revision().clone();

    let ConfigSaveOutcome::Saved(saved) = coordinator.save(&expected, *candidate) else {
        panic!("prepared candidate save failed")
    };

    assert_eq!(fs::read(path).unwrap(), expected_preview.as_bytes());
    assert_eq!(saved.effective_revision, expected_effective);
    assert_eq!(saved.validated_config, saved.validated_config.clone());
}

#[test]
fn load_hashes_exact_disk_bytes_and_returns_a_normalized_preview() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("oxiroute.lua");
    let source = b"return { version = 1, listeners = {} }\n";
    fs::write(&path, source).unwrap();
    let coordinator = CanonicalConfigCoordinator::new(&path).unwrap();

    let document = loaded(coordinator.load());

    assert_eq!(
        document.authored_revision,
        AuthoredRevision::from_bytes(source)
    );
    assert!(document.validated_config.as_draft().listeners.is_empty());
    assert!(
        document
            .validated_config
            .as_draft()
            .rtmp_services
            .is_empty()
    );
    assert_eq!(
        document.effective_revision,
        EffectiveRevision::from_bytes(
            render_config(ConfigFormat::Kdl, &document.validated_config)
                .unwrap()
                .as_bytes()
        )
    );
    assert_ne!(
        document.authored_revision.as_str(),
        document.effective_revision.as_str()
    );
    assert_eq!(
        load_lua(&document.config_preview).unwrap().as_draft(),
        document.validated_config.as_draft()
    );
    assert!(document.diagnostics.is_empty());
}

#[test]
fn canonical_minimal_config_bytes_and_effective_revision_are_stable() {
    let config = minimal_config().validate().unwrap();
    let canonical = render_config(ConfigFormat::Kdl, &config).unwrap();

    assert_eq!(
        canonical,
        concat!(
            "(array)cache_stores {\n}\n",
            "(array)certificates {\n}\n",
            "(array)forward_proxy_services {\n}\n",
            "(array)http_services {\n}\n",
            "(array)l4_services {\n}\n",
            "(array)listeners {\n}\n",
            "management #null\n",
            "max_connections #null\n",
            "registry #null\n",
            "(array)rtmp_services {\n}\n",
            "stats #null\n",
            "(array)tls_profiles {\n}\n",
            "(array)upstream_pools {\n}\n",
            "version 1\n",
        )
    );
    assert_eq!(
        EffectiveRevision::from_bytes(canonical.as_bytes()).as_str(),
        "2267bcdb337bd589b19fa98367ea91b04e857bb81ecb21329c91a7aee32af072"
    );
}

#[test]
fn lua_render_and_parse_failures_keep_their_diagnostic_stages() {
    let oversized: ConfigDraft = serde_json::from_value(serde_json::json!({
        "version": 1,
        "listeners": [],
        "cache_stores": [{
            "type": "memory",
            "name": "x".repeat(1024 * 1024)
        }]
    }))
    .expect("oversized output draft");
    let oversized = oversized.validate().expect("valid oversized output");
    let render_error = render_config(ConfigFormat::Lua, &oversized).unwrap_err();
    let render_diagnostic = source_error_diagnostic(&render_error);
    assert_eq!(render_diagnostic.code, "E_RENDER");
    assert_eq!(render_diagnostic.stage, ConfigDiagnosticStage::Render);

    let representative = ConfigSourceError::Render {
        format: "Lua",
        message: "representative renderer failure".into(),
        source: None,
    };
    let representative_diagnostic = source_error_diagnostic(&representative);
    assert_eq!(representative_diagnostic.code, "E_RENDER");
    assert_eq!(
        representative_diagnostic.stage,
        ConfigDiagnosticStage::Render
    );

    let parse_error = resolve_source_with_format(
        Path::new("broken.lua"),
        b"return { version = 1, listeners = {",
        ConfigFormat::Lua,
    )
    .unwrap_err();
    let parse_diagnostic = source_error_diagnostic(&parse_error);
    assert_eq!(parse_diagnostic.code, "E_SYNTAX");
    assert_eq!(parse_diagnostic.stage, ConfigDiagnosticStage::Parse);
}

#[test]
fn typed_validation_is_normalized_and_deterministic() {
    let (_temp, _path, coordinator) = fixture(&minimal_config());

    let ConfigValidationOutcome::Valid(first) = coordinator.prepare(normalizable_config()) else {
        panic!("first draft is invalid")
    };
    let ConfigValidationOutcome::Valid(second) = coordinator.prepare(normalizable_config()) else {
        panic!("second draft is invalid")
    };
    let first = *first;
    let second = *second;

    assert_eq!(first, second);
    assert_eq!(
        first.validated_config().as_draft().http_services[0].routes[0]
            .host
            .as_ref(),
        Some(&HttpHostSelector::NormalizedHost {
            value: "api.example.test".into(),
        })
    );
    assert_eq!(
        &first.validated_config().as_draft().http_services[0].routes[0].path,
        &HttpPathSelector::SegmentPrefix {
            value: "/v1/".into(),
        }
    );
    assert_eq!(
        render_config(ConfigFormat::Lua, first.validated_config()).unwrap(),
        first.config_preview()
    );
}

#[test]
fn materialized_formats_load_and_save_in_their_authored_format() {
    let directory = TempDir::new().unwrap();
    for (extension, format) in [
        ("kdl", ConfigFormat::Kdl),
        ("lua", ConfigFormat::Lua),
        ("uci", ConfigFormat::Uci),
        ("hocon", ConfigFormat::Hocon),
    ] {
        let path = directory.path().join(format!("oxiroute.{extension}"));
        let config = minimal_config().validate().unwrap();
        let source = render_config(format, &config).unwrap();
        fs::write(&path, &source).unwrap();
        let coordinator = CanonicalConfigCoordinator::new(&path).unwrap();
        let document = loaded(coordinator.load());

        assert_eq!(document.format, format);
        assert!(!document.compositional);
        assert!(document.dependencies.is_empty());
        assert_eq!(document.config_preview, source);
        assert_eq!(
            document.authored_revision,
            AuthoredRevision::from_bytes(source.as_bytes())
        );

        let mut draft = minimal_config();
        draft.max_connections = Some(17);
        let ConfigSaveOutcome::Saved(saved) =
            save_draft(&coordinator, &document.authored_revision, draft.clone())
        else {
            panic!("{format:?} save failed")
        };
        assert_eq!(saved.format, format);
        assert_eq!(fs::read_to_string(&path).unwrap(), saved.config_preview);
        assert_eq!(
            loaded(coordinator.load()).validated_config.as_draft(),
            &draft
        );
    }
}

#[test]
fn revision_parser_requires_lowercase_sha256_values() {
    let lower = "a3".repeat(32);
    let upper = lower.to_ascii_uppercase();
    assert!(lower.parse::<AuthoredRevision>().is_ok());
    assert!(lower.parse::<EffectiveRevision>().is_ok());
    assert!(upper.parse::<AuthoredRevision>().is_err());
    assert!(upper.parse::<EffectiveRevision>().is_err());
    assert!("not-a-revision".parse::<AuthoredRevision>().is_err());
}

fn rejected_diagnostic(name: &str, source: &str) -> ConfigDiagnostic {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join(name);
    fs::write(&path, source).unwrap();
    let ConfigLoadOutcome::Rejected(rejection) =
        CanonicalConfigCoordinator::new(&path).unwrap().load()
    else {
        panic!("source was accepted")
    };
    assert_eq!(rejection.diagnostics.len(), 1);
    rejection.diagnostics.into_iter().next().unwrap()
}

#[test]
fn kdl_syntax_errors_report_the_line_without_quoting_the_source() {
    let diagnostic = rejected_diagnostic(
        "oxiroute.kdl",
        "version 1\nlisteners {\n  name \"secret_external_value\" {{\n",
    );

    assert_eq!(diagnostic.code, "E_SYNTAX");
    assert_eq!(diagnostic.line, Some(3));
    assert!(!format!("{diagnostic:?}").contains("secret_external_value"));
}

#[test]
fn a_missing_version_names_the_field() {
    let diagnostic = rejected_diagnostic("oxiroute.kdl", "(array)listeners {\n}\n");

    assert_eq!(diagnostic.code, "E_INVALID_VALUE", "{diagnostic:?}");
    assert!(
        diagnostic
            .detail
            .is_some_and(|detail| detail.contains("`version`"))
    );
}

#[test]
fn a_rejected_value_is_not_quoted_but_its_setting_is_named() {
    let diagnostic = rejected_diagnostic(
        "oxiroute.kdl",
        "version 1\nmax_connections \"secret_external_value\"\n",
    );
    let detail = diagnostic.detail.expect("detail");

    assert!(detail.contains("expected"));
    assert!(!detail.contains("secret_external_value"));
}

#[test]
fn validation_errors_point_at_the_failing_setting() {
    let mut config = minimal_config();
    config.upstream_pools.push(UpstreamPool {
        name: "farm".into(),
        servers: ["127.0.0.1:42021", "127.0.0.1:42022"]
            .into_iter()
            .map(|address| oxiroute_config::UpstreamServer {
                name: format!("s{}", &address[10..]),
                endpoint: socket_endpoint(address),
                max_connections: None,
                dns_resolution: oxiroute_config::DnsResolutionPolicy::OnConnect,
            })
            .collect(),
        endpoints: Vec::new(),
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: None,
        passive_health: None,
        tls: None,
        http_versions: HttpVersionPolicy::default(),
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::Safe,
        registration: None,
        resilience: oxiroute_config::PoolResilience::default(),
        queue: None,
    });
    let source = render_config(ConfigFormat::Kdl, &config.validate().unwrap())
        .unwrap()
        .replace("127.0.0.1:42022", "127.0.0.1:42021");

    let diagnostic = rejected_diagnostic("oxiroute.kdl", &source);

    assert_eq!(
        diagnostic.path.as_deref(),
        Some("/upstream_pools/farm"),
        "{diagnostic:?}"
    );
    assert!(
        diagnostic
            .detail
            .is_some_and(|detail| detail.contains("duplicate endpoint"))
    );
}
