use std::{net::IpAddr, path::PathBuf};

use oxiroute_config::{
    LexicalError, canonical_certificate_dns_name, canonical_dns_name, canonical_ip,
    normalize_unix_path, validate_file_path,
};

#[test]
fn keeps_endpoint_certificate_and_ip_identities_distinct() {
    assert_eq!(
        canonical_dns_name("Api.Example.Test"),
        Ok("api.example.test".into())
    );
    assert_eq!(
        canonical_dns_name("*.example.test"),
        Err(LexicalError::Wildcard)
    );
    assert_eq!(
        canonical_dns_name("192.0.2.1"),
        Err(LexicalError::IpAddress)
    );

    assert_eq!(
        canonical_certificate_dns_name("*.Example.Test"),
        Ok("*.example.test".into())
    );
    assert_eq!(
        canonical_certificate_dns_name("192.0.2.1"),
        Err(LexicalError::IpAddress)
    );
    assert_eq!(
        canonical_certificate_dns_name("*.192.0.2.1"),
        Err(LexicalError::IpAddress)
    );

    assert_eq!(
        canonical_ip("::ffff:192.0.2.1".parse::<IpAddr>().unwrap()),
        "192.0.2.1".parse::<IpAddr>().unwrap()
    );
}

#[test]
fn reports_stable_dns_failure_categories() {
    let label = "a".repeat(63);
    let too_long = format!("{label}.{label}.{label}.{label}");
    for (value, expected) in [
        ("", LexicalError::Empty),
        ("example.test.", LexicalError::InvalidDnsLabel),
        ("-api.example.test", LexicalError::InvalidDnsLabel),
        ("api..example.test", LexicalError::InvalidDnsLabel),
        ("api_.example.test", LexicalError::InvalidDnsLabel),
    ] {
        assert_eq!(canonical_dns_name(value), Err(expected), "{value:?}");
    }
    assert_eq!(
        canonical_dns_name("caf\u{e9}.example.test"),
        Err(LexicalError::NonAsciiDnsName)
    );
    assert_eq!(canonical_dns_name(&too_long), Err(LexicalError::TooLong));
}

#[test]
fn keeps_file_paths_literal_and_unix_paths_normalized() {
    assert_eq!(
        validate_file_path(std::path::Path::new("/etc/oxiroute/config.lua")),
        Ok(())
    );
    assert_eq!(
        validate_file_path(std::path::Path::new("/etc//oxiroute/config.lua")),
        Err(LexicalError::RepeatedSeparator)
    );
    assert_eq!(
        validate_file_path(std::path::Path::new("etc/oxiroute/config.lua")),
        Err(LexicalError::RelativePath)
    );
    assert_eq!(
        validate_file_path(std::path::Path::new("/etc/oxiroute/../config.lua")),
        Err(LexicalError::DotSegment)
    );

    let mut boundary = PathBuf::from(format!("//{}", "a".repeat(106)));
    assert_eq!(normalize_unix_path(&mut boundary), Ok(()));
    assert_eq!(boundary, PathBuf::from(format!("/{}", "a".repeat(106))));

    let mut too_long = PathBuf::from(format!("//{}", "a".repeat(107)));
    assert_eq!(
        normalize_unix_path(&mut too_long),
        Err(LexicalError::TooLong)
    );
}
