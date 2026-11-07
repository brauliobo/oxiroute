#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn origin(directive: &ExpandedDirective) -> DirectiveOrigin {
    DirectiveOrigin {
        occurrence: directive.occurrence,
        span: directive.directive.span,
        provenance: directive.provenance.clone(),
    }
}

pub(super) fn lower_name(value: &[u8]) -> String {
    String::from_utf8_lossy(value).to_ascii_lowercase()
}

pub(super) fn word_text(word: &Word) -> Option<String> {
    std::str::from_utf8(&word.value).ok().map(str::to_owned)
}

pub(super) fn absolute_path(word: &Word) -> Option<PathBuf> {
    let value = std::str::from_utf8(&word.value).ok()?;
    let path = std::path::Path::new(value);
    (path.is_absolute()
        && !value.is_empty()
        && !value.contains("//")
        && !value.ends_with('/')
        && !value
            .strip_prefix('/')
            .unwrap_or(value)
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == ".."))
    .then(|| path.to_path_buf())
}

pub(super) fn parse_explicit_socket(arguments: &[Word]) -> Option<SocketAddr> {
    if arguments.len() != 1 {
        return None;
    }
    let value = word_text(&arguments[0])?;
    if value.contains('$') {
        return None;
    }
    let value = value
        .strip_prefix("*:")
        .map_or_else(|| value.clone(), |port| format!("0.0.0.0:{port}"));
    let address = value.parse::<SocketAddr>().ok()?;
    (address.port() != 0).then_some(address)
}

pub(super) fn parse_virtual_host_sockets(arguments: &[Word]) -> Option<Vec<SocketAddr>> {
    if arguments.is_empty() {
        return None;
    }
    let mut addresses = Vec::with_capacity(arguments.len());
    for argument in arguments {
        let address = parse_explicit_socket(std::slice::from_ref(argument))?;
        if !addresses.contains(&address) {
            addresses.push(address);
        }
    }
    (!addresses.is_empty()).then_some(addresses)
}

pub(super) fn socket_addresses_overlap(first: SocketAddr, second: SocketAddr) -> bool {
    first.port() == second.port()
        && (first.ip() == second.ip()
            || first.ip().is_unspecified()
            || second.ip().is_unspecified())
}

pub(super) struct ParsedServerName {
    pub(super) selector: HttpHostSelector,
    pub(super) certificate_name: String,
}

pub(super) fn parse_server_name(
    word: &Word,
    listener_port: u16,
) -> Result<ParsedServerName, (DiagnosticCode, String)> {
    let value =
        word_text(word).ok_or_else(|| (E_INVALID_VALUE, "Apache host name is not UTF-8".into()))?;
    parse_server_name_text(&value, listener_port)
}

pub(super) fn parse_server_name_text(
    value: &str,
    listener_port: u16,
) -> Result<ParsedServerName, (DiagnosticCode, String)> {
    if value.is_empty() || has_dynamic(value) || value.contains('*') {
        return Err((
            E_UNSUPPORTED_FEATURE,
            "Apache ServerName/ServerAlias must be an exact static host name".into(),
        ));
    }
    let (host, port) = split_host_port(value).ok_or_else(|| {
        (
            E_INVALID_VALUE,
            "Apache ServerName/ServerAlias is not a canonical host or authority".into(),
        )
    })?;
    let host = canonical_host(&host).ok_or_else(|| {
        (
            E_INVALID_VALUE,
            "Apache ServerName/ServerAlias is not a canonical DNS name or IP address".into(),
        )
    })?;
    if let Some(port) = port {
        if port != listener_port {
            return Err((
                E_SEMANTICS_NOT_REPRESENTABLE,
                "Apache host authority port does not match its VirtualHost listener".into(),
            ));
        }
        let authority = format_authority(&host, port);
        Ok(ParsedServerName {
            selector: HttpHostSelector::AsciiCaseInsensitiveExactAuthority { value: authority },
            certificate_name: host,
        })
    } else {
        Ok(ParsedServerName {
            selector: HttpHostSelector::NormalizedHost {
                value: host.clone(),
            },
            certificate_name: host,
        })
    }
}

pub(super) fn canonical_host(value: &str) -> Option<String> {
    if let Ok(ip) = value.parse::<IpAddr>() {
        return Some(ip.to_string());
    }
    let value = value.strip_suffix('.').unwrap_or(value);
    dns_name(value.as_bytes())
}

pub(super) fn split_host_port(value: &str) -> Option<(String, Option<u16>)> {
    if let Some(rest) = value.strip_prefix('[') {
        let (host, port) = rest.split_once(']')?;
        let port = if let Some(port) = port.strip_prefix(':') {
            Some(port.parse().ok()?)
        } else {
            None
        };
        return Some((host.to_owned(), port));
    }
    let mut pieces = value.rsplitn(2, ':');
    let last = pieces.next()?;
    let first = pieces.next();
    match first {
        Some(host) if last.bytes().all(|byte| byte.is_ascii_digit()) => {
            Some((host.to_owned(), Some(last.parse().ok()?)))
        }
        Some(_) => None,
        None => Some((last.to_owned(), None)),
    }
}

fn format_authority(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

pub(super) fn parse_proxy_pass(
    directive: &ExpandedDirective,
) -> Result<EffectiveProxyPass, (DiagnosticCode, String)> {
    let arguments = &directive.directive.arguments;
    if arguments.len() != 2 {
        return Err((
            E_DYNAMIC_PROXY_PASS,
            "ProxyPass options and interpolation are outside the static importer subset".into(),
        ));
    }
    let path = word_text(&arguments[0])
        .ok_or_else(|| (E_INVALID_VALUE, "ProxyPass path is not UTF-8".into()))?;
    if !path.starts_with('/') || path.contains('?') || path.contains('#') {
        return Err((
            E_DYNAMIC_PROXY_PASS,
            "ProxyPass requires a static absolute request path".into(),
        ));
    }
    let Some(path) = oxiroute_config::canonicalize_http_path(&path)
        .filter(|canonical| canonical.as_ref() == path)
        .map(std::borrow::Cow::into_owned)
    else {
        return Err((
            E_SEMANTICS_NOT_REPRESENTABLE,
            "ProxyPass request path has ambiguous canonical normalization".into(),
        ));
    };
    let target = parse_proxy_target(&arguments[1])?;
    let target_path = match &target {
        ProxyTarget::Direct { target_path, .. } | ProxyTarget::Balancer { target_path, .. } => {
            target_path
        }
    };
    if target_path != &path {
        return Err((
            E_SEMANTICS_NOT_REPRESENTABLE,
            "ProxyPass URI replacement is not represented by the canonical proxy action".into(),
        ));
    }
    Ok(EffectiveProxyPass {
        origin: origin(directive),
        path,
        target,
        inherited: false,
    })
}

fn parse_proxy_target(word: &Word) -> Result<ProxyTarget, (DiagnosticCode, String)> {
    let value = word_text(word)
        .ok_or_else(|| (E_INVALID_VALUE, "ProxyPass destination is not UTF-8".into()))?;
    if has_dynamic(&value) || value.contains('?') || value.contains('#') {
        return Err((
            E_DYNAMIC_PROXY_PASS,
            "ProxyPass destination contains dynamic interpolation or query semantics".into(),
        ));
    }
    if let Some(rest) = value.strip_prefix("balancer://") {
        let (name, path) = split_target_path(rest);
        if name.is_empty() || has_dynamic(name) {
            return Err((
                E_DYNAMIC_PROXY_PASS,
                "ProxyPass balancer name is dynamic".into(),
            ));
        }
        return Ok(ProxyTarget::Balancer {
            name: name.to_owned(),
            target_path: path,
        });
    }
    let (scheme, rest) = if let Some(rest) = value.strip_prefix("http://") {
        (ProxyScheme::Http, rest)
    } else if let Some(rest) = value.strip_prefix("https://") {
        (ProxyScheme::Https, rest)
    } else {
        return Err((
            E_UNSUPPORTED_FEATURE,
            "ProxyPass destination must use static http://, https://, or balancer:// syntax".into(),
        ));
    };
    let (authority, target_path) = split_target_path(rest);
    let (host, explicit_port) = split_host_port(authority).ok_or_else(|| {
        (
            E_INVALID_VALUE,
            "ProxyPass destination authority is invalid".into(),
        )
    })?;
    if host.is_empty() || authority.contains('@') {
        return Err((
            E_INVALID_VALUE,
            "ProxyPass destination authority is invalid".into(),
        ));
    }
    let host = canonical_host(&host).ok_or_else(|| {
        (
            E_INVALID_VALUE,
            "ProxyPass destination host is not a canonical DNS name or IP address".into(),
        )
    })?;
    let port = explicit_port.unwrap_or(match scheme {
        ProxyScheme::Http => 80,
        ProxyScheme::Https => 443,
    });
    if port == 0 {
        return Err((
            E_INVALID_VALUE,
            "ProxyPass destination port is invalid".into(),
        ));
    }
    let endpoint = if let Ok(address) = host.parse::<IpAddr>() {
        UpstreamEndpoint::Socket {
            address: SocketAddr::new(address, port),
        }
    } else {
        UpstreamEndpoint::Dns {
            host: host.clone(),
            port,
        }
    };
    Ok(ProxyTarget::Direct {
        scheme,
        endpoint,
        host,
        port,
        target_path,
    })
}

pub(super) struct ParsedMemberTarget {
    pub(super) scheme: ProxyScheme,
    pub(super) endpoint: UpstreamEndpoint,
    pub(super) host: String,
    pub(super) port: u16,
}

pub(super) fn parse_member_target(word: &Word) -> Result<ParsedMemberTarget, ()> {
    let value = word_text(word).ok_or(())?;
    let (scheme, rest) = if let Some(rest) = value.strip_prefix("http://") {
        (ProxyScheme::Http, rest)
    } else if let Some(rest) = value.strip_prefix("https://") {
        (ProxyScheme::Https, rest)
    } else {
        return Err(());
    };
    let (authority, path) = split_target_path(rest);
    if path != "/" {
        return Err(());
    }
    let (host, explicit_port) = split_host_port(authority).ok_or(())?;
    let host = canonical_host(&host).ok_or(())?;
    let port = explicit_port.unwrap_or(match scheme {
        ProxyScheme::Http => 80,
        ProxyScheme::Https => 443,
    });
    if port == 0 {
        return Err(());
    }
    let endpoint = if let Ok(address) = host.parse::<IpAddr>() {
        UpstreamEndpoint::Socket {
            address: SocketAddr::new(address, port),
        }
    } else {
        UpstreamEndpoint::Dns {
            host: host.clone(),
            port,
        }
    };
    Ok(ParsedMemberTarget {
        scheme,
        endpoint,
        host,
        port,
    })
}

fn split_target_path(value: &str) -> (&str, String) {
    value
        .split_once('/')
        .map_or((value, "/".into()), |(authority, path)| {
            (authority, format!("/{path}"))
        })
}

pub(super) fn selector_key(selector: &HttpHostSelector) -> String {
    match selector {
        HttpHostSelector::NormalizedHost { value } => format!("host:{value}"),
        HttpHostSelector::AsciiCaseInsensitiveExactAuthority { value } => {
            format!("authority:{}", value.to_ascii_lowercase())
        }
        HttpHostSelector::ExactAuthority { value } => format!("authority:{value}"),
        HttpHostSelector::NginxLeadingWildcard { value }
        | HttpHostSelector::NginxLeadingDot { value } => format!("unsupported:{value}"),
    }
}

pub(super) fn supported_module(module: &str) -> bool {
    [
        "authz_core_module",
        "lbmethod_byrequests_module",
        "log_config_module",
        "mpm_event_module",
        "mpm_prefork_module",
        "proxy_balancer_module",
        "proxy_http_module",
        "proxy_module",
        "slotmem_shm_module",
        "socache_shmcb_module",
        "ssl_module",
        "unixd_module",
    ]
    .contains(&module.to_ascii_lowercase().as_str())
}

pub(super) fn has_dynamic(value: &str) -> bool {
    value.contains('$') || value.contains('%') || value.contains("${") || value.contains("#{")
}

impl ProxyScheme {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }
}
