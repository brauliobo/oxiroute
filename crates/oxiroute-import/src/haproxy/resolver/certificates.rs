fn finish_bind_tls(
    options: &BindOptions<'_>,
    occurrence: OccurrenceId,
) -> Result<Option<EffectiveValue<BindTls>>, BindParseError> {
    match (options.ssl, options.certificate) {
        (None, None) if options.alpn.is_none() && options.minimum_version.is_none() => Ok(None),
        (Some(_), Some(certificate)) => {
            let (alpn, _) = options.alpn.clone().ok_or_else(|| {
                BindParseError::Semantic(
                    "HAProxy TLS bind requires an explicit canonical ALPN policy".into(),
                )
            })?;
            let minimum_version = options
                .minimum_version
                .map_or(TlsMinimumVersion::Tls12, |(version, _)| version);
            let tls = load_bind_tls(&certificate.value, alpn, minimum_version)
                .map_err(BindParseError::Semantic)?;
            Ok(Some(EffectiveValue::direct(
                tls,
                occurrence,
                certificate.span,
            )))
        }
        _ => Err(BindParseError::Semantic(
            "HAProxy TLS bind certificate selection is incomplete".into(),
        )),
    }
}

fn parse_tls_alpn(value: &[u8]) -> Option<Vec<TlsAlpn>> {
    let protocols = value
        .split(|byte| *byte == b',')
        .map(|protocol| match protocol {
            b"h2" => Some(TlsAlpn::H2),
            b"http/1.1" => Some(TlsAlpn::Http11),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    matches!(
        protocols.as_slice(),
        [TlsAlpn::Http11 | TlsAlpn::H2] | [TlsAlpn::H2, TlsAlpn::Http11]
    )
    .then_some(protocols)
}

fn load_bind_tls(
    raw_path: &[u8],
    alpn: Vec<TlsAlpn>,
    minimum_version: TlsMinimumVersion,
) -> Result<BindTls, String> {
    let path = std::str::from_utf8(raw_path)
        .map(PathBuf::from)
        .map_err(|_| "HAProxy crt path is not UTF-8".to_owned())?;
    if !path.is_absolute() {
        return Err(
            "HAProxy crt path must be absolute when no representable crt-base is available".into(),
        );
    }
    let items = read_pem_items(&path, "crt PEM")?;
    let (dns_names, leaf_certificate, embedded_private_keys) = certificate_metadata(&path, &items)?;
    if embedded_private_keys != 0 {
        return Err(format!(
            "HAProxy combined certificate/private-key bundle `{}` cannot be preserved by separate canonical file references",
            path.display()
        ));
    }

    let mut private_key_name = path.as_os_str().to_owned();
    private_key_name.push(".key");
    let private_key_path = PathBuf::from(private_key_name);
    validate_sidecar_key(&private_key_path, &leaf_certificate)?;

    Ok(BindTls {
        certificate_chain_path: path,
        private_key_path,
        dns_names,
        alpn,
        minimum_version,
    })
}

fn read_pem_items(
    path: &std::path::Path,
    kind: &str,
) -> Result<Vec<(PemSectionKind, Vec<u8>)>, String> {
    let bytes = read_stable_pem(path, kind)?;
    let mut reader = BufReader::new(bytes.as_slice());
    let mut items = Vec::new();
    while let Some((kind, data)) = pem::from_buf(&mut reader)
        .map_err(|error| format!("cannot parse HAProxy {kind} `{}`: {error}", path.display()))?
    {
        if matches!(
            kind,
            PemSectionKind::Certificate
                | PemSectionKind::PublicKey
                | PemSectionKind::RsaPrivateKey
                | PemSectionKind::PrivateKey
                | PemSectionKind::EcPrivateKey
                | PemSectionKind::Crl
                | PemSectionKind::Csr
        ) {
            items.push((kind, data));
        }
    }
    Ok(items)
}

fn certificate_metadata(
    path: &std::path::Path,
    items: &[(PemSectionKind, Vec<u8>)],
) -> Result<(Vec<String>, Vec<u8>, usize), String> {
    let mut dns_names = Vec::new();
    let mut leaf_certificate = None;
    let mut certificate_count = 0usize;
    let mut private_key_count = 0usize;
    for (kind, data) in items {
        match kind {
            PemSectionKind::Certificate => {
                if certificate_count == 0 {
                    leaf_certificate = Some(data.clone());
                }
                collect_certificate_metadata(path, data, certificate_count, &mut dns_names)?;
                certificate_count += 1;
            }
            PemSectionKind::RsaPrivateKey
            | PemSectionKind::PrivateKey
            | PemSectionKind::EcPrivateKey => private_key_count += 1,
            _ => {
                return Err(format!(
                    "HAProxy crt PEM `{}` contains an unsupported PEM item",
                    path.display()
                ));
            }
        }
    }
    if certificate_count == 0 {
        return Err(format!(
            "HAProxy crt PEM `{}` contains no certificates",
            path.display()
        ));
    }
    if certificate_count > MAX_CERTIFICATES_IN_CHAIN {
        return Err(format!(
            "HAProxy crt PEM `{}` exceeds {MAX_CERTIFICATES_IN_CHAIN} certificates",
            path.display()
        ));
    }
    validate_dns_identities(path, &dns_names)?;
    Ok((
        dns_names,
        leaf_certificate.expect("nonempty chain has a leaf certificate"),
        private_key_count,
    ))
}

fn collect_certificate_metadata(
    path: &std::path::Path,
    certificate: &[u8],
    index: usize,
    dns_names: &mut Vec<String>,
) -> Result<(), String> {
    let (remainder, parsed) = parse_x509_certificate(certificate).map_err(|_| {
        format!(
            "HAProxy crt PEM `{}` contains an invalid X.509 certificate",
            path.display()
        )
    })?;
    if !remainder.is_empty() {
        return Err(format!(
            "HAProxy crt PEM `{}` contains trailing certificate DER data",
            path.display()
        ));
    }
    let is_ca = parsed
        .basic_constraints()
        .map_err(|_| {
            format!(
                "HAProxy crt PEM `{}` has invalid basic constraints",
                path.display()
            )
        })?
        .is_some_and(|constraints| constraints.value.ca);
    if index != 0 && !is_ca {
        return Err(format!(
            "HAProxy crt PEM `{}` contains multiple end-entity certificates; multi-cert selection is unsupported",
            path.display()
        ));
    }
    if index == 0
        && let Some(names) = parsed.subject_alternative_name().map_err(|_| {
            format!(
                "HAProxy crt PEM `{}` has an invalid subject alternative name extension",
                path.display()
            )
        })?
    {
        for name in &names.value.general_names {
            let GeneralName::DNSName(name) = name else {
                continue;
            };
            let canonical = canonical_certificate_dns_name(name).map_err(|_| {
                format!(
                    "HAProxy crt PEM `{}` contains an unsupported DNS subject alternative name",
                    path.display()
                )
            })?;
            dns_names.push(canonical);
        }
    }
    Ok(())
}

fn validate_dns_identities(path: &std::path::Path, dns_names: &[String]) -> Result<(), String> {
    if dns_names.is_empty() {
        return Err(format!(
            "HAProxy crt PEM `{}` has no DNS subject alternative names",
            path.display()
        ));
    }
    if dns_names.len() > MAX_CERTIFICATE_DNS_NAMES {
        return Err(format!(
            "HAProxy crt PEM `{}` exceeds {MAX_CERTIFICATE_DNS_NAMES} DNS subject alternative names",
            path.display()
        ));
    }
    let mut unique = std::collections::HashSet::with_capacity(dns_names.len());
    if dns_names.iter().all(|name| unique.insert(name)) {
        Ok(())
    } else {
        Err(format!(
            "HAProxy crt PEM `{}` repeats a DNS subject alternative name",
            path.display()
        ))
    }
}

fn validate_sidecar_key(path: &std::path::Path, leaf_certificate: &[u8]) -> Result<(), String> {
    let bytes = Zeroizing::new(read_stable_pem(path, "crt sidecar key")?);
    let mut reader = BufReader::new(bytes.as_slice());
    let mut items = Vec::new();
    while let Some((kind, data)) = pem::from_buf(&mut reader).map_err(|error| {
        format!(
            "cannot parse HAProxy crt sidecar key `{}`: {error}",
            path.display()
        )
    })? {
        if matches!(
            kind,
            PemSectionKind::Certificate
                | PemSectionKind::PublicKey
                | PemSectionKind::RsaPrivateKey
                | PemSectionKind::PrivateKey
                | PemSectionKind::EcPrivateKey
                | PemSectionKind::Crl
                | PemSectionKind::Csr
        ) {
            items.push((kind, data));
        }
    }
    let key_count = items
        .iter()
        .filter(|(kind, _)| {
            matches!(
                *kind,
                PemSectionKind::RsaPrivateKey
                    | PemSectionKind::PrivateKey
                    | PemSectionKind::EcPrivateKey
            )
        })
        .count();
    if key_count != items.len() {
        return Err(format!(
            "HAProxy crt sidecar key `{}` contains a non-key PEM item",
            path.display()
        ));
    }
    if key_count != 1 {
        return Err(format!(
            "HAProxy crt sidecar key `{}` must contain exactly one private key",
            path.display()
        ));
    }
    let private_key = PKey::private_key_from_pem(&bytes).map_err(|_| {
        format!(
            "HAProxy crt sidecar key `{}` is not a supported private key",
            path.display()
        )
    })?;
    let minimum_bits = match private_key.id() {
        Id::RSA | Id::RSA_PSS => 2_048,
        Id::EC => 256,
        _ => {
            return Err(format!(
                "HAProxy crt sidecar key `{}` uses an unsupported algorithm",
                path.display()
            ));
        }
    };
    if private_key.bits() < minimum_bits {
        return Err(format!(
            "HAProxy crt sidecar key `{}` is below the minimum key strength",
            path.display()
        ));
    }
    let certificate = X509::from_der(leaf_certificate).map_err(|_| {
        format!(
            "HAProxy crt PEM `{}` has an invalid leaf certificate",
            path.display()
        )
    })?;
    let public_key = certificate.public_key().map_err(|_| {
        format!(
            "HAProxy crt PEM for `{}` has no supported public key",
            path.display()
        )
    })?;
    if !public_key.public_eq(&private_key) {
        return Err(format!(
            "HAProxy crt sidecar key `{}` does not match the leaf certificate",
            path.display()
        ));
    }
    Ok(())
}

fn read_stable_pem(path: &std::path::Path, kind: &str) -> Result<Vec<u8>, String> {
    let mut file = File::open(path)
        .map_err(|error| format!("cannot open HAProxy {kind} `{}`: {error}", path.display()))?;
    let before = file.metadata().map_err(|error| {
        format!(
            "cannot inspect HAProxy {kind} `{}`: {error}",
            path.display()
        )
    })?;
    if !before.is_file() {
        return Err(format!(
            "HAProxy {kind} `{}` is not a regular file",
            path.display()
        ));
    }
    if before.len() > u64::try_from(MAX_CERTIFICATE_CHAIN_BYTES).unwrap_or(u64::MAX) {
        return Err(pem_size_error(path, kind));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(
            u64::try_from(MAX_CERTIFICATE_CHAIN_BYTES)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        )
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read HAProxy {kind} `{}`: {error}", path.display()))?;
    let after = file.metadata().map_err(|error| {
        format!(
            "cannot re-inspect HAProxy {kind} `{}`: {error}",
            path.display()
        )
    })?;
    if PemFingerprint::new(&before) != PemFingerprint::new(&after) {
        return Err(format!(
            "HAProxy {kind} `{}` changed while metadata was read",
            path.display()
        ));
    }
    if bytes.len() > MAX_CERTIFICATE_CHAIN_BYTES {
        return Err(pem_size_error(path, kind));
    }
    Ok(bytes)
}

fn pem_size_error(path: &std::path::Path, kind: &str) -> String {
    format!(
        "HAProxy {kind} `{}` exceeds {MAX_CERTIFICATE_CHAIN_BYTES} bytes",
        path.display()
    )
}

#[derive(Eq, PartialEq)]
struct PemFingerprint {
    length: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
}

impl PemFingerprint {
    fn new(metadata: &Metadata) -> Self {
        Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            changed_seconds: metadata.ctime(),
            #[cfg(unix)]
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

