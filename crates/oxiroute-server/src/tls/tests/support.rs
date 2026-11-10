#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn config_with_identity(files: &IdentityFiles) -> ConfigDraft {
    ConfigDraft {
        listener_workers: None,
        retirement: None,
        version: 1,
        max_connections: None,
        management: None,
        registry: None,
        stats: None,
        certificates: vec![Certificate {
            name: "primary".into(),
            dns_names: vec!["www.example.test".into()],
            source: CertificateSource::Files {
                certificate_chain_path: files.chain.clone(),
                private_key_path: files.key.clone(),
            },
        }],
        tls_profiles: vec![TlsProfile {
            name: "public".into(),
            certificates: vec!["primary".into()],
            default_certificate: "primary".into(),
            min_version: TlsVersion::Tls12,
            alpn: vec![AlpnProtocol::H2, AlpnProtocol::Http11],
            policy: TlsPolicy::default(),
        }],
        listeners: Vec::new(),
        upstream_pools: Vec::new(),
        http_services: Vec::new(),
        cache_stores: Vec::new(),
        forward_proxy_services: Vec::new(),
        rtmp_services: Vec::new(),
        l4_services: Vec::new(),
        audit: None,
    }
}

#[cfg(unix)]
pub(super) fn wait_for_condition(description: &str, mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !condition() && std::time::Instant::now() < deadline {
        thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(condition(), "timed out waiting for {description}");
}

#[cfg(unix)]
pub(super) struct CertbotTestLineage {
    pub(super) name: String,
    pub(super) live: PathBuf,
    pub(super) archive: PathBuf,
}

#[cfg(unix)]
pub(super) fn config_with_certbot_lineage(files: &CertbotTestLineage) -> ConfigDraft {
    let mut config = config_with_identity(&IdentityFiles {
        chain: PathBuf::new(),
        key: PathBuf::new(),
        ca: PathBuf::new(),
    });
    config.certificates[0].dns_names = vec!["certbot.example.test".into()];
    config.certificates[0].source = CertificateSource::Certbot {
        live_directory_path: files.live.clone(),
        archive_directory_path: files.archive.clone(),
    };
    config
}

#[cfg(unix)]
pub(super) fn write_certbot_lineage(root: &Path, name: &str, dns_name: &str) -> CertbotTestLineage {
    let material = root.join(format!("{name}-material"));
    fs::create_dir(&material).unwrap();
    let identity = write_identity(&material, name, dns_name, false);
    let live = root.join("live").join(name);
    let archive = root.join("archive").join(name);
    fs::create_dir_all(&live).unwrap();
    fs::create_dir_all(&archive).unwrap();

    write_certbot_revision(&archive, 1, &identity);
    let files = CertbotTestLineage {
        name: name.into(),
        live,
        archive,
    };
    set_live_revision(&files, 1);
    files
}

#[cfg(unix)]
fn write_certbot_revision(archive: &Path, revision: u64, identity: &IdentityFiles) {
    let fullchain = fs::read(&identity.chain).unwrap();
    let certificates = X509::stack_from_pem(&fullchain).unwrap();
    let cert = certificates[0].to_pem().unwrap();
    let chain = certificates[1..]
        .iter()
        .flat_map(|certificate| certificate.to_pem().unwrap())
        .collect::<Vec<_>>();

    fs::write(archive.join(format!("cert{revision}.pem")), cert).unwrap();
    fs::write(archive.join(format!("chain{revision}.pem")), chain).unwrap();
    fs::write(archive.join(format!("fullchain{revision}.pem")), fullchain).unwrap();
    write_private_key(
        &archive.join(format!("privkey{revision}.pem")),
        &fs::read(&identity.key).unwrap(),
    );
}

#[cfg(unix)]
pub(super) fn write_new_certbot_revision(
    root: &Path,
    files: &CertbotTestLineage,
    revision: u64,
    stem: &str,
    dns_name: &str,
) {
    let material = root.join(format!("{stem}-material"));
    fs::create_dir(&material).unwrap();
    let identity = write_identity(&material, stem, dns_name, false);
    write_certbot_revision(&files.archive, revision, &identity);
}

#[cfg(unix)]
pub(super) fn copy_certbot_revision(archive: &Path, from: u64, to: u64, reuse_key: bool) {
    use std::os::unix::fs::symlink;

    for stem in ["cert", "chain", "fullchain"] {
        fs::copy(
            archive.join(format!("{stem}{from}.pem")),
            archive.join(format!("{stem}{to}.pem")),
        )
        .unwrap();
    }
    let target = archive.join(format!("privkey{to}.pem"));
    if reuse_key {
        symlink(format!("privkey{from}.pem"), target).unwrap();
    } else {
        write_private_key(
            &target,
            &fs::read(archive.join(format!("privkey{from}.pem"))).unwrap(),
        );
    }
}

#[cfg(unix)]
pub(super) fn set_live_revision(files: &CertbotTestLineage, revision: u64) {
    for stem in ["cert", "chain", "fullchain", "privkey"] {
        set_live_link(
            files,
            &format!("{stem}.pem"),
            &format!("{stem}{revision}.pem"),
        );
    }
}

#[cfg(unix)]
pub(super) fn set_live_link(files: &CertbotTestLineage, name: &str, archive_name: &str) {
    set_raw_live_link(
        &files.live,
        name,
        &Path::new("../../archive")
            .join(&files.name)
            .join(archive_name),
    );
}

#[cfg(unix)]
pub(super) fn set_raw_live_link(live: &Path, name: &str, target: &Path) {
    use std::os::unix::fs::symlink;

    let link = live.join(name);
    if fs::symlink_metadata(&link).is_ok() {
        fs::remove_file(&link).unwrap();
    }
    symlink(target, link).unwrap();
}

pub(super) fn upstream_pool(
    ca_certificate_path: Option<PathBuf>,
    min: HttpVersion,
    max: HttpVersion,
) -> UpstreamPool {
    UpstreamPool {
        name: "origin".into(),
        servers: Vec::new(),
        endpoints: vec![UpstreamEndpoint::Socket {
            address: SocketAddr::from((Ipv4Addr::LOCALHOST, 443)),
        }],
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: None,
        passive_health: None,
        tls: Some(UpstreamTls {
            server_name: "Origin.Example.Test".into(),
            ca_certificate_path,
        }),
        http_versions: HttpVersionPolicy { min, max },
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
        registration: None,
        canary: None,
        resilience: oxiroute_config::PoolResilience::default(),
        queue: None,
    }
}

pub(super) fn write_identity(
    directory: &Path,
    stem: &str,
    dns_name: &str,
    expired: bool,
) -> IdentityFiles {
    let ca_key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let ca = build_certificate("Test Root", None, &ca_key, &ca_key, &[], false, true, false);
    let leaf_key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let leaf = build_certificate(
        dns_name,
        Some(&ca),
        &ca_key,
        &leaf_key,
        &[dns_name],
        expired,
        false,
        true,
    );

    write_identity_material(directory, stem, &leaf, &[&ca], &leaf_key)
}

pub(super) fn write_identity_material(
    directory: &Path,
    stem: &str,
    leaf: &X509,
    issuers: &[&X509],
    leaf_key: &PKey<Private>,
) -> IdentityFiles {
    let chain = directory.join(format!("{stem}-chain.pem"));
    let key = directory.join(format!("{stem}-key.pem"));
    let ca_path = directory.join(format!("{stem}-ca.pem"));
    let mut chain_pem = leaf.to_pem().unwrap();
    for issuer in issuers {
        chain_pem.extend_from_slice(&issuer.to_pem().unwrap());
    }
    fs::write(&chain, chain_pem).unwrap();
    write_private_key(&key, &leaf_key.private_key_to_pem_pkcs8().unwrap());
    fs::write(
        &ca_path,
        issuers
            .last()
            .expect("identity has an issuer")
            .to_pem()
            .unwrap(),
    )
    .unwrap();
    IdentityFiles {
        chain,
        key,
        ca: ca_path,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_certificate(
    common_name: &str,
    issuer: Option<&X509>,
    issuer_key: &PKey<Private>,
    subject_key: &PKey<Private>,
    dns_names: &[&str],
    expired: bool,
    is_ca: bool,
    server_auth: bool,
) -> X509 {
    build_certificate_with_sans(
        common_name,
        issuer,
        issuer_key,
        subject_key,
        dns_names,
        &[],
        expired,
        is_ca,
        server_auth,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_certificate_with_sans(
    common_name: &str,
    issuer: Option<&X509>,
    issuer_key: &PKey<Private>,
    subject_key: &PKey<Private>,
    dns_names: &[&str],
    ip_addresses: &[&str],
    expired: bool,
    is_ca: bool,
    server_auth: bool,
) -> X509 {
    build_certificate_with_leaf_usage(
        common_name,
        issuer,
        issuer_key,
        subject_key,
        dns_names,
        ip_addresses,
        expired,
        is_ca,
        server_auth,
        LeafKeyUsage::DigitalSignature,
    )
}

#[derive(Clone, Copy)]
pub(super) enum LeafKeyUsage {
    DigitalSignature,
    KeyEncipherment,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_certificate_with_leaf_usage(
    common_name: &str,
    issuer: Option<&X509>,
    issuer_key: &PKey<Private>,
    subject_key: &PKey<Private>,
    dns_names: &[&str],
    ip_addresses: &[&str],
    expired: bool,
    is_ca: bool,
    server_auth: bool,
    leaf_key_usage: LeafKeyUsage,
) -> X509 {
    let mut name = X509NameBuilder::new().unwrap();
    name.append_entry_by_text("CN", common_name).unwrap();
    let name = name.build();
    let mut builder = X509::builder().unwrap();
    builder.set_version(2).unwrap();
    let serial = Asn1Integer::from_bn(&BigNum::from_u32(1).unwrap()).unwrap();
    builder.set_serial_number(&serial).unwrap();
    builder.set_subject_name(&name).unwrap();
    builder
        .set_issuer_name(issuer.map_or(name.as_ref(), |certificate| certificate.subject_name()))
        .unwrap();
    builder.set_pubkey(subject_key).unwrap();
    let not_before = Asn1Time::from_str_x509("20200101000000Z").unwrap();
    let not_after = Asn1Time::from_str_x509(if expired {
        "20210101000000Z"
    } else {
        "20490101000000Z"
    })
    .unwrap();
    builder.set_not_before(&not_before).unwrap();
    builder.set_not_after(&not_after).unwrap();
    if is_ca {
        builder
            .append_extension(BasicConstraints::new().critical().ca().build().unwrap())
            .unwrap();
        builder
            .append_extension(
                KeyUsage::new()
                    .critical()
                    .key_cert_sign()
                    .crl_sign()
                    .build()
                    .unwrap(),
            )
            .unwrap();
    } else {
        builder
            .append_extension(BasicConstraints::new().critical().build().unwrap())
            .unwrap();
        let mut key_usage = KeyUsage::new();
        key_usage.critical();
        match leaf_key_usage {
            LeafKeyUsage::DigitalSignature => key_usage.digital_signature(),
            LeafKeyUsage::KeyEncipherment => key_usage.key_encipherment(),
        };
        builder
            .append_extension(key_usage.build().unwrap())
            .unwrap();
        let extended_key_usage = if server_auth {
            ExtendedKeyUsage::new().server_auth().build().unwrap()
        } else {
            ExtendedKeyUsage::new().client_auth().build().unwrap()
        };
        builder.append_extension(extended_key_usage).unwrap();
    }
    let subject_key_identifier = SubjectKeyIdentifier::new()
        .build(&builder.x509v3_context(issuer.map(AsRef::as_ref), None))
        .unwrap();
    builder.append_extension(subject_key_identifier).unwrap();
    let authority_key_identifier = AuthorityKeyIdentifier::new()
        .keyid(true)
        .issuer(true)
        .build(&builder.x509v3_context(issuer.map(AsRef::as_ref), None))
        .unwrap();
    builder.append_extension(authority_key_identifier).unwrap();
    if !dns_names.is_empty() || !ip_addresses.is_empty() {
        let mut subject_alternative_name = SubjectAlternativeName::new();
        for dns_name in dns_names {
            subject_alternative_name.dns(dns_name);
        }
        for ip_address in ip_addresses {
            subject_alternative_name.ip(ip_address);
        }
        let extension = subject_alternative_name
            .build(&builder.x509v3_context(issuer.map(AsRef::as_ref), None))
            .unwrap();
        builder.append_extension(extension).unwrap();
    }
    builder.sign(issuer_key, MessageDigest::sha256()).unwrap();
    builder.build()
}

pub(super) fn write_private_key(path: &Path, pem: &[u8]) {
    fs::write(path, pem).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}
