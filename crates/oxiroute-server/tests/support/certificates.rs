pub struct TestOnlyEcdsaChain {
    pub fullchain_path: PathBuf,
    pub leaf_private_key_path: PathBuf,
    pub root_certificate_path: PathBuf,
    pub intermediate_certificate_path: PathBuf,
    pub leaf_der: Vec<u8>,
    _directory: TempDir,
}

pub struct TestCertbotLineage {
    name: String,
    pub live_directory_path: PathBuf,
    pub archive_directory_path: PathBuf,
    _directory: TempDir,
}

impl TestCertbotLineage {
    pub fn new(name: &str, initial: &TestOnlyEcdsaChain) -> Self {
        let directory = TempDir::new().expect("test Certbot lineage directory");
        let live_directory_path = directory.path().join("live").join(name);
        let archive_directory_path = directory.path().join("archive").join(name);
        fs::create_dir_all(&live_directory_path).expect("create test Certbot live directory");
        fs::create_dir_all(&archive_directory_path).expect("create test Certbot archive directory");
        let lineage = Self {
            name: name.into(),
            live_directory_path,
            archive_directory_path,
            _directory: directory,
        };
        lineage.write_revision(1, initial);
        lineage.activate(1);
        lineage
    }

    pub fn source(&self) -> CertificateSource {
        CertificateSource::Certbot {
            live_directory_path: self.live_directory_path.clone(),
            archive_directory_path: self.archive_directory_path.clone(),
        }
    }

    pub fn write_revision(&self, revision: u64, material: &TestOnlyEcdsaChain) {
        use std::os::unix::fs::PermissionsExt as _;

        let fullchain = fs::read(&material.fullchain_path).expect("read Certbot test fullchain");
        let certificates = X509::stack_from_pem(&fullchain).expect("parse Certbot test fullchain");
        let cert = certificates[0].to_pem().expect("encode Certbot test leaf");
        let chain = certificates[1..]
            .iter()
            .flat_map(|certificate| certificate.to_pem().expect("encode Certbot test issuer"))
            .collect::<Vec<_>>();
        fs::write(
            self.archive_directory_path
                .join(format!("cert{revision}.pem")),
            cert,
        )
        .expect("write Certbot test leaf");
        fs::write(
            self.archive_directory_path
                .join(format!("chain{revision}.pem")),
            chain,
        )
        .expect("write Certbot test chain");
        fs::write(
            self.archive_directory_path
                .join(format!("fullchain{revision}.pem")),
            fullchain,
        )
        .expect("write Certbot test fullchain");
        let key_path = self
            .archive_directory_path
            .join(format!("privkey{revision}.pem"));
        fs::copy(&material.leaf_private_key_path, &key_path)
            .expect("write Certbot test private key");
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))
            .expect("secure Certbot test private key");
    }

    pub fn activate(&self, revision: u64) {
        use std::os::unix::fs::symlink;

        for stem in ["cert", "chain", "fullchain", "privkey"] {
            let link = self.live_directory_path.join(format!("{stem}.pem"));
            if fs::symlink_metadata(&link).is_ok() {
                fs::remove_file(&link).expect("remove prior Certbot test link");
            }
            symlink(
                Path::new("../../archive")
                    .join(&self.name)
                    .join(format!("{stem}{revision}.pem")),
                link,
            )
            .expect("write Certbot test link");
        }
    }
}

pub fn generate_test_only_ecdsa_chain(server_name: &str) -> TestOnlyEcdsaChain {
    generate_test_only_chain(server_name, false)
}

pub fn generate_test_only_client_chain(client_name: &str) -> TestOnlyEcdsaChain {
    generate_test_only_chain(client_name, true)
}

fn generate_test_only_chain(leaf_name: &str, client_auth: bool) -> TestOnlyEcdsaChain {
    let directory = TempDir::new().expect("test-only ECDSA fixture directory");
    let root_key = test_only_ec_key();
    let root_name = test_only_name("OxiRoute Wire Test-Only ECDSA Root");
    let root = test_only_root(&root_key, &root_name);
    let intermediate_key = test_only_ec_key();
    let intermediate_name = test_only_name("OxiRoute Wire Test-Only ECDSA Intermediate");
    let intermediate =
        test_only_intermediate(&intermediate_key, &intermediate_name, &root, &root_key);
    let leaf_key = test_only_ec_key();
    let leaf = test_only_leaf(
        leaf_name,
        &leaf_key,
        &intermediate,
        &intermediate_key,
        client_auth,
    );

    let fullchain_path = directory
        .path()
        .join("wire-test-only-ecdsa-leaf-fullchain.pem");
    let leaf_private_key_path = directory
        .path()
        .join("wire-test-only-ecdsa-leaf-private-key.pem");
    let root_certificate_path = directory
        .path()
        .join("wire-test-only-ecdsa-root-certificate.pem");
    let intermediate_certificate_path = directory
        .path()
        .join("wire-test-only-ecdsa-intermediate-certificate.pem");
    let mut fullchain = leaf.to_pem().expect("test-only ECDSA leaf PEM");
    fullchain.extend_from_slice(
        &intermediate
            .to_pem()
            .expect("test-only ECDSA intermediate PEM"),
    );
    fs::write(&fullchain_path, fullchain).expect("write test-only ECDSA fullchain");
    fs::write(
        &leaf_private_key_path,
        leaf_key
            .private_key_to_pem_pkcs8()
            .expect("test-only ECDSA leaf private key PEM"),
    )
    .expect("write test-only ECDSA leaf private key");
    {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(&leaf_private_key_path, fs::Permissions::from_mode(0o600))
            .expect("secure test-only ECDSA private key");
    }
    fs::write(
        &root_certificate_path,
        root.to_pem().expect("test-only ECDSA root PEM"),
    )
    .expect("write test-only ECDSA root certificate");
    fs::write(
        &intermediate_certificate_path,
        intermediate
            .to_pem()
            .expect("test-only ECDSA intermediate PEM"),
    )
    .expect("write test-only ECDSA intermediate certificate");

    TestOnlyEcdsaChain {
        fullchain_path,
        leaf_private_key_path,
        root_certificate_path,
        intermediate_certificate_path,
        leaf_der: leaf.to_der().expect("test-only ECDSA leaf DER"),
        _directory: directory,
    }
}

fn test_only_ec_key() -> PKey<Private> {
    let group =
        EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).expect("test-only ECDSA P-256 group");
    let key = EcKey::generate(&group).expect("test-only ECDSA key");
    PKey::from_ec_key(key).expect("test-only ECDSA PKey")
}

fn test_only_name(common_name: &str) -> X509Name {
    let mut name = X509NameBuilder::new().expect("test-only certificate name");
    name.append_entry_by_text("CN", common_name)
        .expect("test-only certificate common name");
    name.build()
}

fn test_only_serial(value: u32) -> Asn1Integer {
    Asn1Integer::from_bn(&BigNum::from_u32(value).expect("test-only serial number"))
        .expect("test-only ASN.1 serial number")
}

fn test_only_root(key: &PKey<Private>, name: &X509Name) -> X509 {
    let mut certificate = X509::builder().expect("test-only root builder");
    certificate.set_version(2).expect("test-only root version");
    certificate
        .set_serial_number(&test_only_serial(0x4001))
        .expect("test-only root serial");
    certificate
        .set_subject_name(name)
        .expect("test-only root subject");
    certificate
        .set_issuer_name(name)
        .expect("test-only root issuer");
    certificate.set_pubkey(key).expect("test-only root key");
    set_test_only_validity(&mut certificate);
    certificate
        .append_extension(
            BasicConstraints::new()
                .critical()
                .ca()
                .build()
                .expect("test-only root basic constraints"),
        )
        .expect("append test-only root basic constraints");
    certificate
        .append_extension(
            KeyUsage::new()
                .critical()
                .key_cert_sign()
                .crl_sign()
                .build()
                .expect("test-only root key usage"),
        )
        .expect("append test-only root key usage");
    let subject_key_identifier = SubjectKeyIdentifier::new()
        .build(&certificate.x509v3_context(None, None))
        .expect("test-only root subject key identifier");
    certificate
        .append_extension(subject_key_identifier)
        .expect("append test-only root subject key identifier");
    certificate
        .sign(key, MessageDigest::sha256())
        .expect("sign test-only root");
    certificate.build()
}

fn test_only_intermediate(
    key: &PKey<Private>,
    name: &X509Name,
    root: &X509,
    root_key: &PKey<Private>,
) -> X509 {
    let mut certificate = X509::builder().expect("test-only intermediate builder");
    certificate
        .set_version(2)
        .expect("test-only intermediate version");
    certificate
        .set_serial_number(&test_only_serial(0x4002))
        .expect("test-only intermediate serial");
    certificate
        .set_subject_name(name)
        .expect("test-only intermediate subject");
    certificate
        .set_issuer_name(root.subject_name())
        .expect("test-only intermediate issuer");
    certificate
        .set_pubkey(key)
        .expect("test-only intermediate key");
    set_test_only_validity(&mut certificate);
    certificate
        .append_extension(
            BasicConstraints::new()
                .critical()
                .ca()
                .pathlen(0)
                .build()
                .expect("test-only intermediate basic constraints"),
        )
        .expect("append test-only intermediate basic constraints");
    certificate
        .append_extension(
            KeyUsage::new()
                .critical()
                .key_cert_sign()
                .crl_sign()
                .build()
                .expect("test-only intermediate key usage"),
        )
        .expect("append test-only intermediate key usage");
    let subject_key_identifier = SubjectKeyIdentifier::new()
        .build(&certificate.x509v3_context(Some(root), None))
        .expect("test-only intermediate subject key identifier");
    certificate
        .append_extension(subject_key_identifier)
        .expect("append test-only intermediate subject key identifier");
    let authority_key_identifier = AuthorityKeyIdentifier::new()
        .keyid(true)
        .build(&certificate.x509v3_context(Some(root), None))
        .expect("test-only intermediate authority key identifier");
    certificate
        .append_extension(authority_key_identifier)
        .expect("append test-only intermediate authority key identifier");
    certificate
        .sign(root_key, MessageDigest::sha256())
        .expect("sign test-only intermediate");
    certificate.build()
}

fn test_only_leaf(
    leaf_name: &str,
    key: &PKey<Private>,
    intermediate: &X509,
    intermediate_key: &PKey<Private>,
    client_auth: bool,
) -> X509 {
    let name = test_only_name(leaf_name);
    let mut certificate = X509::builder().expect("test-only ECDSA leaf builder");
    certificate
        .set_version(2)
        .expect("test-only ECDSA leaf version");
    certificate
        .set_serial_number(&test_only_serial(0x4003))
        .expect("test-only ECDSA leaf serial");
    certificate
        .set_subject_name(&name)
        .expect("test-only ECDSA leaf subject");
    certificate
        .set_issuer_name(intermediate.subject_name())
        .expect("test-only ECDSA leaf issuer");
    certificate
        .set_pubkey(key)
        .expect("test-only ECDSA leaf key");
    set_test_only_validity(&mut certificate);
    certificate
        .append_extension(
            BasicConstraints::new()
                .critical()
                .build()
                .expect("test-only ECDSA leaf basic constraints"),
        )
        .expect("append test-only ECDSA leaf basic constraints");
    certificate
        .append_extension(
            KeyUsage::new()
                .critical()
                .digital_signature()
                .build()
                .expect("test-only ECDSA leaf key usage"),
        )
        .expect("append test-only ECDSA leaf key usage");
    let mut extended_key_usage = ExtendedKeyUsage::new();
    if client_auth {
        extended_key_usage.client_auth();
    } else {
        extended_key_usage.server_auth();
    }
    certificate
        .append_extension(
            extended_key_usage
                .build()
                .expect("test-only ECDSA leaf extended key usage"),
        )
        .expect("append test-only ECDSA leaf extended key usage");
    let subject_alternative_name = SubjectAlternativeName::new()
        .dns(leaf_name)
        .build(&certificate.x509v3_context(Some(intermediate), None))
        .expect("test-only ECDSA leaf SAN");
    certificate
        .append_extension(subject_alternative_name)
        .expect("append test-only ECDSA leaf SAN");
    let subject_key_identifier = SubjectKeyIdentifier::new()
        .build(&certificate.x509v3_context(Some(intermediate), None))
        .expect("test-only ECDSA leaf subject key identifier");
    certificate
        .append_extension(subject_key_identifier)
        .expect("append test-only ECDSA leaf subject key identifier");
    let authority_key_identifier = AuthorityKeyIdentifier::new()
        .keyid(true)
        .build(&certificate.x509v3_context(Some(intermediate), None))
        .expect("test-only ECDSA leaf authority key identifier");
    certificate
        .append_extension(authority_key_identifier)
        .expect("append test-only ECDSA leaf authority key identifier");
    certificate
        .sign(intermediate_key, MessageDigest::sha256())
        .expect("sign test-only ECDSA leaf");
    certificate.build()
}

fn set_test_only_validity(certificate: &mut openssl::x509::X509Builder) {
    certificate
        .set_not_before(&Asn1Time::days_from_now(0).expect("test-only not before"))
        .expect("set test-only not before");
    certificate
        .set_not_after(&Asn1Time::days_from_now(30).expect("test-only not after"))
        .expect("set test-only not after");
}
