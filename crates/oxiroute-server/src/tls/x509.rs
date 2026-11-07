#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) fn certificate_is_ca_capable(
    certificate: &openssl::x509::X509Ref,
) -> Result<bool, openssl::error::ErrorStack> {
    let der = certificate.to_der()?;
    Ok(
        ca_extensions(&der).is_some_and(|(basic_constraints, key_usage)| {
            basic_constraints && key_usage.unwrap_or(true)
        }),
    )
}

pub(crate) enum CertificateIdentitySans {
    Missing,
    Malformed,
    Names(Vec<CertificateIdentitySan>),
}

pub(crate) enum CertificateIdentitySan {
    Dns(Vec<u8>),
    Ip(IpAddr),
}

pub(crate) fn certificate_identity_sans(
    certificate: &openssl::x509::X509Ref,
) -> Result<CertificateIdentitySans, openssl::error::ErrorStack> {
    const SUBJECT_ALT_NAME_OID: &[u8] = &[0x55, 0x1d, 0x11];

    let der = certificate.to_der()?;
    let mut extensions = match certificate_extensions(&der) {
        ParsedExtensions::Missing => return Ok(CertificateIdentitySans::Missing),
        ParsedExtensions::Malformed => return Ok(CertificateIdentitySans::Malformed),
        ParsedExtensions::Present(extensions) => extensions,
    };
    let mut identities = None;
    while !extensions.is_empty() {
        let Some((oid, value)) = der_extension(&mut extensions) else {
            return Ok(CertificateIdentitySans::Malformed);
        };
        if oid != SUBJECT_ALT_NAME_OID {
            continue;
        }
        if identities.is_some() {
            return Ok(CertificateIdentitySans::Malformed);
        }
        let mut value = value;
        let Some(mut general_names) = der_value(&mut value, 0x30) else {
            return Ok(CertificateIdentitySans::Malformed);
        };
        if !value.is_empty() {
            return Ok(CertificateIdentitySans::Malformed);
        }
        let mut parsed_identities = Vec::new();
        while !general_names.is_empty() {
            let Some((tag, value)) = der_element(&mut general_names) else {
                return Ok(CertificateIdentitySans::Malformed);
            };
            match tag {
                0x82 => parsed_identities.push(CertificateIdentitySan::Dns(value.to_vec())),
                0x87 => {
                    let ip = match value {
                        [a, b, c, d] => IpAddr::V4(Ipv4Addr::new(*a, *b, *c, *d)),
                        value if value.len() == 16 => {
                            let octets: [u8; 16] = value
                                .try_into()
                                .expect("a 16-byte IP SAN is an IPv6 address");
                            IpAddr::V6(Ipv6Addr::from(octets))
                        }
                        _ => return Ok(CertificateIdentitySans::Malformed),
                    };
                    parsed_identities.push(CertificateIdentitySan::Ip(ip));
                }
                _ => {}
            }
        }
        identities = Some(parsed_identities);
    }
    Ok(identities.map_or(
        CertificateIdentitySans::Missing,
        CertificateIdentitySans::Names,
    ))
}

enum ParsedExtensions<'a> {
    Missing,
    Malformed,
    Present(&'a [u8]),
}

fn certificate_extensions(der: &[u8]) -> ParsedExtensions<'_> {
    let Some(certificate) = (|| {
        let mut certificate_der = der;
        let certificate = der_value(&mut certificate_der, 0x30)?;
        certificate_der.is_empty().then_some(certificate)
    })() else {
        return ParsedExtensions::Malformed;
    };

    let Some(mut tbs_certificate) = ({
        let mut certificate = certificate;
        der_value(&mut certificate, 0x30)
    }) else {
        return ParsedExtensions::Malformed;
    };
    if tbs_certificate.first() == Some(&0xa0) && der_element(&mut tbs_certificate).is_none() {
        return ParsedExtensions::Malformed;
    }
    for _ in 0..6 {
        if der_element(&mut tbs_certificate).is_none() {
            return ParsedExtensions::Malformed;
        }
    }

    while !tbs_certificate.is_empty() {
        let Some((tag, value)) = der_element(&mut tbs_certificate) else {
            return ParsedExtensions::Malformed;
        };
        if tag != 0xa3 {
            continue;
        }
        let mut explicit_extensions = value;
        let Some(extensions) = der_value(&mut explicit_extensions, 0x30) else {
            return ParsedExtensions::Malformed;
        };
        if !explicit_extensions.is_empty() || !tbs_certificate.is_empty() {
            return ParsedExtensions::Malformed;
        }
        return ParsedExtensions::Present(extensions);
    }
    ParsedExtensions::Missing
}

fn ca_extensions(der: &[u8]) -> Option<(bool, Option<bool>)> {
    let ParsedExtensions::Present(extensions) = certificate_extensions(der) else {
        return None;
    };
    parse_ca_extensions(extensions)
}

fn parse_ca_extensions(mut extensions: &[u8]) -> Option<(bool, Option<bool>)> {
    const BASIC_CONSTRAINTS_OID: &[u8] = &[0x55, 0x1d, 0x13];
    const KEY_USAGE_OID: &[u8] = &[0x55, 0x1d, 0x0f];

    let mut basic_constraints = None;
    let mut key_usage = None;
    while !extensions.is_empty() {
        let (oid, value) = der_extension(&mut extensions)?;

        match oid {
            BASIC_CONSTRAINTS_OID if basic_constraints.is_none() => {
                basic_constraints = Some(parse_basic_constraints(value)?);
            }
            KEY_USAGE_OID if key_usage.is_none() => {
                key_usage = Some(parse_key_usage(value)?);
            }
            BASIC_CONSTRAINTS_OID | KEY_USAGE_OID => return None,
            _ => {}
        }
    }

    basic_constraints.map(|basic_constraints| (basic_constraints, key_usage))
}

fn der_extension<'a>(extensions: &mut &'a [u8]) -> Option<(&'a [u8], &'a [u8])> {
    let mut extension = der_value(extensions, 0x30)?;
    let oid = der_value(&mut extension, 0x06)?;
    if extension.first() == Some(&0x01) {
        der_value(&mut extension, 0x01)?;
    }
    let value = der_value(&mut extension, 0x04)?;
    extension.is_empty().then_some((oid, value))
}

fn parse_basic_constraints(value: &[u8]) -> Option<bool> {
    let mut value = value;
    let mut constraints = der_value(&mut value, 0x30)?;
    if !value.is_empty() {
        return None;
    }
    if constraints.is_empty() {
        return Some(false);
    }
    let ca = der_value(&mut constraints, 0x01)?;
    (ca.len() == 1).then_some(ca[0] != 0)
}

fn parse_key_usage(value: &[u8]) -> Option<bool> {
    let mut value = value;
    let key_usage = der_value(&mut value, 0x03)?;
    if !value.is_empty() || key_usage.len() < 2 || key_usage[0] > 7 {
        return None;
    }
    Some(key_usage[1] & 0x04 != 0)
}

fn der_value<'a>(input: &mut &'a [u8], expected_tag: u8) -> Option<&'a [u8]> {
    let (tag, value) = der_element(input)?;
    (tag == expected_tag).then_some(value)
}

fn der_element<'a>(input: &mut &'a [u8]) -> Option<(u8, &'a [u8])> {
    let (&tag, remainder) = input.split_first()?;
    let (&length_byte, mut remainder) = remainder.split_first()?;
    let length = if length_byte & 0x80 == 0 {
        usize::from(length_byte)
    } else {
        let length_bytes = usize::from(length_byte & 0x7f);
        if length_bytes == 0 || length_bytes > size_of::<usize>() || remainder.len() < length_bytes
        {
            return None;
        }
        let (encoded_length, rest) = remainder.split_at(length_bytes);
        if encoded_length[0] == 0 {
            return None;
        }
        remainder = rest;
        encoded_length.iter().try_fold(0_usize, |length, byte| {
            length.checked_mul(256)?.checked_add(usize::from(*byte))
        })?
    };
    let (value, remainder) = remainder.split_at_checked(length)?;
    *input = remainder;
    Some((tag, value))
}

pub(crate) fn pem_labels<'a>(
    owner: &str,
    kind: &'static str,
    path: &Path,
    bytes: &'a [u8],
) -> Result<Vec<&'a str>, TlsBuildError> {
    let text = std::str::from_utf8(bytes).map_err(|_| TlsBuildError::InvalidPem {
        owner: owner.into(),
        kind,
        path: path.into(),
        detail: "PEM must be ASCII text",
    })?;
    let mut labels = Vec::new();
    let mut open_label = None;

    for raw_line in text.lines() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if let Some(label) = open_label {
            let expected_end = format!("-----END {label}-----");
            if line == expected_end {
                labels.push(label);
                open_label = None;
            } else if line.starts_with("-----BEGIN ") || line.starts_with("-----END ") {
                return Err(TlsBuildError::InvalidPem {
                    owner: owner.into(),
                    kind,
                    path: path.into(),
                    detail: "nested or mismatched PEM boundary",
                });
            }
            continue;
        }

        if line.trim().is_empty() {
            continue;
        }
        let Some(label) = line
            .strip_prefix("-----BEGIN ")
            .and_then(|line| line.strip_suffix("-----"))
        else {
            return Err(TlsBuildError::InvalidPem {
                owner: owner.into(),
                kind,
                path: path.into(),
                detail: "non-PEM data outside a PEM block",
            });
        };
        if label.is_empty() {
            return Err(TlsBuildError::InvalidPem {
                owner: owner.into(),
                kind,
                path: path.into(),
                detail: "empty PEM label",
            });
        }
        open_label = Some(label);
    }

    if open_label.is_some() {
        return Err(TlsBuildError::InvalidPem {
            owner: owner.into(),
            kind,
            path: path.into(),
            detail: "unterminated PEM block",
        });
    }
    if labels.is_empty() {
        return Err(TlsBuildError::InvalidPem {
            owner: owner.into(),
            kind,
            path: path.into(),
            detail: "no PEM blocks",
        });
    }
    Ok(labels)
}
