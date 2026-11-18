#[allow(clippy::wildcard_imports)]
use super::*;
use oxiroute_config::{
    Certificate, CertificateSource, HttpAccessPolicy, Management, Registry, RtmpAutoPushPolicy,
    SelfSignedKeyType, Stats, TlsClientAuthMode, TlsPolicy, TlsProfile, UpstreamTls,
};

fn rewriting_the_file_changes_the_revision(set: impl Fn(&mut ConfigDraft, &Path)) {
    let directory = TempDir::new().expect("external directory");
    let file = directory.path().join("external");
    fs::write(&file, "one").unwrap();
    let mut config = normalizable_config();
    set(&mut config, &file);
    let (_temp, _path, coordinator) = fixture(&config);

    let first = coordinator.load_shared().unwrap();
    fs::write(&file, "two").unwrap();
    let second = coordinator.load_shared().unwrap();
    fs::write(&file, "one").unwrap();
    let restored = coordinator.load_shared().unwrap();
    fs::remove_file(&file).unwrap();
    let missing = coordinator.load_shared().unwrap();

    assert_eq!(first.authored_revision, second.authored_revision);
    assert_ne!(first.effective_revision, second.effective_revision);
    assert_eq!(first.effective_revision, restored.effective_revision);
    assert_ne!(first.effective_revision, missing.effective_revision);
    assert_ne!(second.effective_revision, missing.effective_revision);
    assert_eq!(coordinator.lock().resolutions, 1);
}

fn management(token_file: &Path) -> Management {
    Management {
        bind: "127.0.0.1:9900".parse().unwrap(),
        ui_dir: None,
        token_file: Some(token_file.to_path_buf()),
    }
}

fn with_tls_profile(config: &mut ConfigDraft, policy: TlsPolicy) {
    config.certificates.push(Certificate {
        name: "dev".into(),
        dns_names: vec!["api.example.test".into()],
        source: CertificateSource::SelfSignedDevelopment {
            validity_days: 30,
            key_type: SelfSignedKeyType::default(),
        },
    });
    config.tls_profiles.push(TlsProfile {
        name: "edge".into(),
        certificates: vec!["dev".into()],
        default_certificate: "dev".into(),
        min_version: oxiroute_config::TlsVersion::default(),
        alpn: vec![oxiroute_config::AlpnProtocol::Http11],
        policy,
    });
    config.listeners[0].tls_profile = Some("edge".into());
}

#[test]
fn the_management_token_file_is_part_of_the_revision() {
    rewriting_the_file_changes_the_revision(|config, file| {
        config.management = Some(management(file));
    });
}

#[test]
fn the_stats_admin_token_file_is_part_of_the_revision() {
    rewriting_the_file_changes_the_revision(|config, file| {
        config.stats = Some(Stats {
            binds: vec!["127.0.0.1:9901".parse().unwrap()],
            admin_token_file: Some(file.to_path_buf()),
            pages: Vec::new(),
        });
    });
}

#[test]
fn the_registry_tokens_file_is_part_of_the_revision() {
    rewriting_the_file_changes_the_revision(|config, file| {
        config.registry = Some(Registry {
            state_dir: file.parent().unwrap().to_path_buf(),
            tokens_file: Some(file.to_path_buf()),
            endpoints: Vec::new(),
        });
    });
}

#[test]
fn an_http_route_token_file_is_part_of_the_revision() {
    rewriting_the_file_changes_the_revision(|config, file| {
        config.http_services[0].routes[0].access_policy = Some(HttpAccessPolicy::BearerTokenFile {
            token_file_path: file.to_path_buf(),
            header_name: "authorization".into(),
            realm: None,
        });
    });
}

#[test]
fn an_upstream_ca_file_is_part_of_the_revision() {
    rewriting_the_file_changes_the_revision(|config, file| {
        config.upstream_pools[0].tls = Some(UpstreamTls {
            server_name: Some("origin.example.test".into()),
            verify: oxiroute_config::UpstreamTlsVerify::Full,
            ca_certificate_path: Some(file.to_path_buf()),
        });
    });
}

#[test]
fn an_rtmp_secret_file_is_part_of_the_revision() {
    rewriting_the_file_changes_the_revision(|config, file| {
        *config = rtmp_config("rtmp");
        config.rtmp_services[0].auto_push = RtmpAutoPushPolicy {
            enabled: true,
            secret_file: Some(file.to_path_buf()),
            ..RtmpAutoPushPolicy::default()
        };
    });
}

#[test]
fn a_dh_parameters_file_is_part_of_the_revision() {
    rewriting_the_file_changes_the_revision(|config, file| {
        with_tls_profile(
            config,
            TlsPolicy {
                dh_parameters_path: Some(file.to_path_buf()),
                ..TlsPolicy::default()
            },
        );
    });
}

#[test]
fn a_client_ca_file_is_part_of_the_revision() {
    rewriting_the_file_changes_the_revision(|config, file| {
        let mut policy = TlsPolicy::default();
        policy.client_auth.mode = TlsClientAuthMode::Required;
        policy.client_auth.ca_certificate_path = Some(file.to_path_buf());
        with_tls_profile(config, policy);
    });
}

#[test]
fn a_candidate_prepared_from_a_draft_carries_the_same_revision_as_the_file() {
    let directory = TempDir::new().expect("external directory");
    let file = directory.path().join("token");
    fs::write(&file, "one").unwrap();
    let mut config = normalizable_config();
    config.management = Some(management(&file));
    let (_temp, _path, coordinator) = fixture(&config);

    let loaded = coordinator.load_shared().unwrap();
    let ConfigValidationOutcome::Valid(candidate) = coordinator.prepare(config) else {
        panic!("candidate rejected")
    };

    assert_eq!(candidate.effective_revision(), &loaded.effective_revision);
}

#[test]
fn a_configuration_without_external_files_keeps_the_revision_of_its_canonical_form() {
    let (_temp, _path, coordinator) = fixture(&normalizable_config());

    let document = coordinator.load_shared().unwrap();

    assert_eq!(
        document.effective_revision,
        EffectiveRevision::from_bytes(
            render_config(ConfigFormat::Kdl, &document.validated_config)
                .unwrap()
                .as_bytes()
        )
    );
}

#[test]
fn every_path_in_the_model_is_a_named_external_input_or_not_one() {
    const NOT_EXTERNAL: [&str; 18] = [
        "root_directory",
        "socket_dir",
        "executable",
        "working_directory",
        "state_dir",
        "state_root",
        "dir",
        "ui_dir",
        "path",
        "spa_fallback",
        "file",
        "certificate_chain_path",
        "private_key_path",
        "live_directory_path",
        "archive_directory_path",
        "token_file",
        "root_directory_mut",
        "relative_path",
    ];
    let model = Path::new(env!("CARGO_MANIFEST_DIR")).join("../oxiroute-config/src/model");
    let mut unnamed = Vec::new();
    let mut found = std::collections::BTreeSet::new();
    for entry in fs::read_dir(model).unwrap() {
        let source = fs::read_to_string(entry.unwrap().path()).unwrap();
        for line in source.lines().map(str::trim) {
            let Some((name, kind)) = line.split_once(": ") else {
                continue;
            };
            let name = name.rsplit(' ').next().unwrap_or(name);
            if !(kind.starts_with("PathBuf") || kind.starts_with("Option<PathBuf>")) {
                continue;
            }
            found.insert(name.to_owned());
            if !external::EXTERNAL_FILE_KEYS.contains(&name) && !NOT_EXTERNAL.contains(&name) {
                unnamed.push(name.to_owned());
            }
        }
    }
    assert!(
        unnamed.is_empty(),
        "classify {unnamed:?} as an external input of the revision or not"
    );
    for key in external::EXTERNAL_FILE_KEYS {
        assert!(found.contains(key), "{key} is no field of the model");
    }
}
