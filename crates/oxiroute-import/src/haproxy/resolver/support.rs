fn parse_host_port(value: &[u8]) -> Option<(Vec<u8>, u16)> {
    if value.starts_with(b"[") {
        let closing = value.iter().position(|byte| *byte == b']')?;
        if value.get(closing + 1) != Some(&b':') {
            return None;
        }
        let host = value.get(1..closing)?.to_vec();
        let port = parse_u16(value.get(closing + 2..)?)?;
        return (port != 0).then_some((host, port));
    }
    let colon = value.iter().rposition(|byte| *byte == b':')?;
    let host = value[..colon].to_vec();
    let port = parse_u16(value.get(colon + 1..)?)?;
    (port != 0).then_some((host, port))
}

fn is_supported_section(kind: SectionKind) -> bool {
    matches!(
        kind,
        SectionKind::Global
            | SectionKind::Defaults
            | SectionKind::Frontend
            | SectionKind::Backend
            | SectionKind::Listen
    )
}

fn supports_bind(kind: SectionKind) -> bool {
    matches!(kind, SectionKind::Frontend | SectionKind::Listen)
}

const fn supports_stats_page(kind: SectionKind) -> bool {
    matches!(kind, SectionKind::Frontend | SectionKind::Listen)
}

fn supports_default_backend(kind: SectionKind) -> bool {
    matches!(
        kind,
        SectionKind::Defaults | SectionKind::Frontend | SectionKind::Listen
    )
}

fn supports_balance(kind: SectionKind) -> bool {
    matches!(
        kind,
        SectionKind::Defaults | SectionKind::Backend | SectionKind::Listen
    )
}

fn supports_server(kind: SectionKind) -> bool {
    matches!(kind, SectionKind::Backend | SectionKind::Listen)
}

fn supports_backend_policy(kind: SectionKind) -> bool {
    matches!(
        kind,
        SectionKind::Defaults | SectionKind::Backend | SectionKind::Listen
    )
}

fn supports_maxconn(kind: SectionKind) -> bool {
    matches!(
        kind,
        SectionKind::Defaults | SectionKind::Frontend | SectionKind::Listen
    )
}

fn supports_use_backend(kind: SectionKind) -> bool {
    matches!(kind, SectionKind::Frontend | SectionKind::Listen)
}

fn supports_http_rules(kind: SectionKind) -> bool {
    matches!(
        kind,
        SectionKind::Defaults | SectionKind::Frontend | SectionKind::Backend | SectionKind::Listen
    )
}

fn is_known_resolver_directive(name: &[u8]) -> bool {
    matches!(
        name,
        b"bind"
            | b"default_backend"
            | b"balance"
            | b"server"
            | b"retries"
            | b"maxconn"
            | b"use_backend"
            | b"http-check"
            | b"http-request"
            | b"http-response"
    )
}

fn is_global_security_directive(name: &[u8]) -> bool {
    matches!(
        name,
        b"ca-base"
            | b"crt-base"
            | b"hard-stop-after"
            | b"ssl-default-bind-ciphers"
            | b"ssl-default-bind-ciphersuites"
            | b"ssl-default-bind-curves"
            | b"ssl-default-bind-options"
            | b"ssl-default-server-ciphers"
            | b"ssl-default-server-ciphersuites"
            | b"ssl-default-server-curves"
            | b"ssl-default-server-options"
            | b"ssl-dh-param-file"
            | b"tune.ssl.cachesize"
            | b"tune.ssl.default-dh-param"
            | b"tune.ssl.lifetime"
    )
}

fn is_proxy_default_directive(name: &[u8]) -> bool {
    matches!(
        name,
        b"dispatch"
            | b"fullconn"
            | b"hash-type"
            | b"http-reuse"
            | b"http-send-name-header"
            | b"load-server-state-from-file"
            | b"server-state-file"
            | b"server-template"
            | b"source"
            | b"transparent"
    )
}

fn is_conditional(directive: &Directive) -> bool {
    matches!(
        directive.name.value.as_slice(),
        b".if" | b".elif" | b".else" | b".endif"
    )
}

fn is_process_owned(name: &[u8]) -> bool {
    matches!(
        name,
        b"chroot"
            | b"cpu-map"
            | b"daemon"
            | b"group"
            | b"master-worker"
            | b"nbproc"
            | b"nbthread"
            | b"pidfile"
            | b"setgid"
            | b"setuid"
            | b"user"
    )
}

fn process_requirement_kind(name: &[u8]) -> DeploymentRequirementKind {
    match name {
        b"user" | b"setuid" => DeploymentRequirementKind::ProcessUser,
        b"group" | b"setgid" => DeploymentRequirementKind::ProcessGroup,
        b"chroot" => DeploymentRequirementKind::Chroot,
        b"daemon" | b"master-worker" | b"pidfile" => DeploymentRequirementKind::Daemonization,
        _ => DeploymentRequirementKind::WorkerModel,
    }
}

fn is_logging_directive(directive: &Directive) -> bool {
    is_logging_directive_name(&directive.name.value)
        || (directive.name.value == b"option"
            && directive
                .arguments
                .first()
                .is_some_and(|option| is_logging_option(&option.value)))
}

fn is_logging_directive_name(name: &[u8]) -> bool {
    matches!(
        name,
        b"log" | b"log-format" | b"error-log-format" | b"unique-id-format" | b"unique-id-header"
    )
}

fn is_logging_option(name: &[u8]) -> bool {
    matches!(name, b"dontlognull" | b"httplog" | b"logasap" | b"tcplog")
}

fn section_name(kind: SectionKind) -> &'static str {
    match kind {
        SectionKind::Global => "global",
        SectionKind::Defaults => "defaults",
        SectionKind::Frontend => "frontend",
        SectionKind::Backend => "backend",
        SectionKind::Listen => "listen",
        SectionKind::Userlist => "userlist",
        SectionKind::Peers => "peers",
        SectionKind::Mailers => "mailers",
        SectionKind::NamespaceList => "namespace_list",
        SectionKind::Traces => "traces",
        SectionKind::Resolvers => "resolvers",
        SectionKind::Cache => "cache",
        SectionKind::FcgiApp => "fcgi-app",
        SectionKind::Ring => "ring",
        SectionKind::LogForward => "log-forward",
        SectionKind::LogProfile => "log-profile",
        SectionKind::HttpErrors => "http-errors",
        SectionKind::CrtStore => "crt-store",
        SectionKind::Acme => "acme",
        SectionKind::Healthcheck => "healthcheck",
        SectionKind::Program => "program",
    }
}

fn display_bytes(value: &[u8]) -> String {
    String::from_utf8_lossy(value).into_owned()
}

fn display_directive(directive: &Directive) -> String {
    std::iter::once(directive.name.value.as_slice())
        .chain(
            directive
                .arguments
                .iter()
                .map(|argument| argument.value.as_slice()),
        )
        .map(display_bytes)
        .collect::<Vec<_>>()
        .join(" ")
}

fn exact_prometheus_exporter(directive: &Directive) -> bool {
    directive
        .arguments
        .iter()
        .map(|argument| argument.value.as_slice())
        .eq([
            b"use-service".as_slice(),
            b"prometheus-exporter".as_slice(),
            b"if".as_slice(),
            b"{".as_slice(),
            b"path".as_slice(),
            b"/metrics".as_slice(),
            b"}".as_slice(),
        ])
}
