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
    let kind = "HAProxy crt PEM";
    let PemMetadata {
        identities: dns_names,
        leaf: leaf_certificate,
        private_keys,
    } = certificate_metadata(&path, kind, &read_pem_items(&path, kind)?)?;
    if private_keys != 0 {
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

fn validate_sidecar_key(path: &std::path::Path, leaf_certificate: &[u8]) -> Result<(), String> {
    let bytes = Zeroizing::new(read_stable_pem(path, "HAProxy crt sidecar key")?);
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
