use oxiroute_config::{ConfigError, Protocol};

use crate::common::{changed, error_from};

#[test]
fn rejects_unsupported_versions() {
    let error = error_from(&changed("  version = 1,", "  version = 2,"));

    assert!(matches!(error, ConfigError::UnsupportedVersion(2)));
}

#[test]
fn rejects_non_loopback_and_zero_port_management_binds() {
    let error = error_from(&changed("127.0.0.1:9080", "0.0.0.0:9080"));
    assert!(matches!(error, ConfigError::ManagementMustUseLoopback(_)));

    let error = error_from(&changed("127.0.0.1:9080", "127.0.0.1:0"));
    assert!(matches!(
        error,
        ConfigError::ZeroPort {
            kind: "management listener",
            name,
            field: "bind"
        } if name == "management"
    ));
}

#[test]
fn rejects_blank_names_in_every_namespace() {
    let cases = [
        (
            "      name = \"web-certificate\",\n      dns_names",
            "      name = \"  \",\n      dns_names",
            "certificate",
        ),
        (
            "      name = \"web-tls\",\n      certificate",
            "      name = \"  \",\n      certificate",
            "TLS profile",
        ),
        (
            "      name = \"web\",\n      bind",
            "      name = \"  \",\n      bind",
            "listener",
        ),
        (
            "      name = \"web-backends\",",
            "      name = \"  \",",
            "upstream pool",
        ),
        (
            "      name = \"web\",\n      routes",
            "      name = \"  \",\n      routes",
            "HTTP service",
        ),
        (
            "      name = \"database\",\n      upstream_pool",
            "      name = \"  \",\n      upstream_pool",
            "L4 service",
        ),
    ];

    for (from, to, expected_namespace) in cases {
        let error = error_from(&changed(from, to));
        assert!(matches!(
            error,
            ConfigError::BlankName { namespace, index: 0 }
                if namespace == expected_namespace
        ));
    }
}

#[test]
fn rejects_names_with_surrounding_whitespace_or_control_characters() {
    for name in [" web ", "web\\nedge"] {
        let error = error_from(&changed(
            "      name = \"web\",\n      bind",
            &format!("      name = \"{name}\",\n      bind"),
        ));
        assert!(matches!(
            error,
            ConfigError::InvalidName {
                namespace: "listener",
                index: 0,
                ..
            }
        ));
    }
}

#[test]
fn rejects_duplicate_names_in_every_namespace() {
    let source = changed(
        "  },\n  tls_profiles = {",
        r#"    {
      name = "web-certificate",
      dns_names = { "duplicate.example.test" },
      source = {
        type = "files",
        certificate_chain_path = "/etc/oxiroute/duplicate-chain.pem",
        private_key_path = "/etc/oxiroute/duplicate-key.pem",
      },
    },
  },
  tls_profiles = {"#,
    );
    let error = error_from(&source);
    assert!(matches!(
        error,
        ConfigError::DuplicateName { namespace: "certificate", name }
            if name == "web-certificate"
    ));

    let source = changed(
        "  },\n  listeners = {",
        r#"    {
      name = "web-tls",
      certificates = { "web-certificate" },
      default_certificate = "web-certificate",
    },
  },
  listeners = {"#,
    );
    let error = error_from(&source);
    assert!(matches!(
        error,
        ConfigError::DuplicateName { namespace: "TLS profile", name } if name == "web-tls"
    ));

    let error = error_from(&changed(
        "      name = \"database\",\n      bind",
        "      name = \"web\",\n      bind",
    ));
    assert!(matches!(
        error,
        ConfigError::DuplicateName { namespace: "listener", name } if name == "web"
    ));

    let error = error_from(&changed(
        "      name = \"database-backends\",",
        "      name = \"web-backends\",",
    ));
    assert!(matches!(
        error,
        ConfigError::DuplicateName { namespace: "upstream pool", name }
            if name == "web-backends"
    ));

    let source = changed(
        "      max_request_body_bytes = 2097152,\n    },\n  },\n  rtmp_services = {",
        r#"      max_request_body_bytes = 2097152,
    },
    {
      name = "web",
      routes = {
        {
          path = { kind = "exact", value = "/duplicate" },
          action = { type = "fixed_response", status = 200 },
        },
      },
    },
  },
  rtmp_services = {"#,
    );
    let error = error_from(&source);
    assert!(matches!(
        error,
        ConfigError::DuplicateName { namespace: "HTTP service", name } if name == "web"
    ));

    let source = changed(
        "      lifetime_timeout_ms = 600000,\n    },",
        r#"      lifetime_timeout_ms = 600000,
    },
    {
      name = "database",
      upstream_pool = "database-backends",
    },"#,
    );
    let error = error_from(&source);
    assert!(matches!(
        error,
        ConfigError::DuplicateName { namespace: "L4 service", name } if name == "database"
    ));
}

#[test]
fn rejects_overlapping_and_zero_port_listener_binds() {
    let error = error_from(&changed("127.0.0.1:15432", "127.0.0.1:8080"));
    assert!(matches!(
        error,
        ConfigError::OverlappingBind {
            first_name,
            second_name,
            ..
        } if first_name == "web" && second_name == "database"
    ));

    let error = error_from(&changed("127.0.0.1:8080", "0.0.0.0:15432"));
    assert!(matches!(
        error,
        ConfigError::OverlappingBind { first_name, second_name, .. }
            if first_name == "web" && second_name == "database"
    ));

    let error = error_from(&changed("127.0.0.1:8080", "127.0.0.1:9080"));
    assert!(matches!(
        error,
        ConfigError::OverlappingBind { first_name, second_name, .. }
            if first_name == "management" && second_name == "web"
    ));

    let error = error_from(&changed("127.0.0.1:8080", "[::ffff:127.0.0.1]:15432"));
    assert!(matches!(
        error,
        ConfigError::OverlappingBind { first_name, second_name, .. }
            if first_name == "web" && second_name == "database"
    ));

    let error = error_from(&changed("127.0.0.1:8080", "[::ffff:127.0.0.1]:9080"));
    assert!(matches!(
        error,
        ConfigError::OverlappingBind { first_name, second_name, .. }
            if first_name == "management" && second_name == "web"
    ));

    let error = error_from(&changed("127.0.0.1:8080", "127.0.0.1:0"));
    assert!(matches!(
        error,
        ConfigError::ZeroPort {
            kind: "listener",
            name,
            field: "bind"
        } if name == "web"
    ));
}

#[test]
fn rejects_zero_listener_connection_limits() {
    let error = error_from(&changed(
        "      max_connections = 5000,",
        "      max_connections = 0,",
    ));

    assert!(matches!(
        error,
        ConfigError::ZeroLimit {
            kind: "listener",
            name,
            field: "max_connections"
        } if name == "web"
    ));
}

#[test]
fn rejects_listener_limits_that_json_cannot_represent_exactly() {
    let error = error_from(&changed(
        "      max_connections = 5000,",
        "      max_connections = 9007199254740992,",
    ));

    assert!(matches!(
        error,
        ConfigError::LimitTooLarge {
            kind: "listener",
            name,
            field: "max_connections"
        } if name == "web"
    ));
}

#[test]
fn requires_every_listener_to_reference_a_same_kind_service() {
    let cases = [
        ("      service = \"web\",\n", Protocol::Http, "web"),
        ("      service = \"database\",\n", Protocol::Tcp, "database"),
        ("      service = \"live\",\n", Protocol::Rtmp, "live"),
    ];

    for (field, protocol, listener) in cases {
        let error = error_from(&changed(field, ""));
        assert!(matches!(
            error,
            ConfigError::MissingListenerService {
                listener: actual_listener,
                protocol: actual_protocol,
            } if actual_listener == listener && actual_protocol == protocol
        ));
    }
}

#[test]
fn requires_listeners_to_reference_same_kind_services() {
    let cases = [
        (
            "      service = \"web\",",
            "      service = \"database\",",
            Protocol::Http,
            "web",
        ),
        (
            "      service = \"database\",",
            "      service = \"web\",",
            Protocol::Tcp,
            "database",
        ),
        (
            "      service = \"live\",",
            "      service = \"web\",",
            Protocol::Rtmp,
            "live",
        ),
    ];

    for (from, to, protocol, listener) in cases {
        let error = error_from(&changed(from, to));
        assert!(matches!(
            error,
            ConfigError::UnknownListenerService {
                listener: actual_listener,
                protocol: actual_protocol,
                ..
            } if actual_listener == listener && actual_protocol == protocol
        ));
    }
}

#[test]
fn rejects_zero_http_service_limits() {
    let cases = [
        (
            "      upstream_io_timeout_ms = 15000,",
            "      upstream_io_timeout_ms = 0,",
            "upstream_io_timeout_ms",
        ),
        (
            "      max_request_body_bytes = 2097152,",
            "      max_request_body_bytes = 0,",
            "max_request_body_bytes",
        ),
    ];

    for (from, to, expected_field) in cases {
        let error = error_from(&changed(from, to));
        assert!(matches!(
            error,
            ConfigError::ZeroLimit {
                kind: "HTTP service",
                name,
                field
            } if name == "web" && field == expected_field
        ));
    }
}

#[test]
fn rejects_zero_l4_service_timeouts() {
    let cases = [
        (
            "      connect_timeout_ms = 5000,",
            "      connect_timeout_ms = 0,",
            "connect_timeout_ms",
        ),
        (
            "      idle_timeout_ms = 120000,",
            "      idle_timeout_ms = 0,",
            "idle_timeout_ms",
        ),
        (
            "      lifetime_timeout_ms = 600000,",
            "      lifetime_timeout_ms = 0,",
            "lifetime_timeout_ms",
        ),
    ];

    for (from, to, expected_field) in cases {
        let error = error_from(&changed(from, to));
        assert!(matches!(
            error,
            ConfigError::ZeroLimit {
                kind: "L4 service",
                name,
                field
            } if name == "database" && field == expected_field
        ));
    }
}
