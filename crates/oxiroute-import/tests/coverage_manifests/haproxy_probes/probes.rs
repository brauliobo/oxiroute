#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn haproxy_http_form(id: &str) -> bool {
    id.ends_with("-http")
        || id
            .rsplit_once('.')
            .is_some_and(|(_, suffix)| suffix == "http")
        || id.starts_with("directive.haproxy.acl.")
        || id.starts_with("directive.haproxy.http-request.")
        || id.starts_with("directive.haproxy.http-response.")
        || matches!(
            id,
            "directive.haproxy.use-backend"
                | "directive.haproxy.use-backend.host-path-conjunction"
                | "directive.haproxy.use-backend.dynamic-expression"
                | "directive.haproxy.option.redispatch-bare-http"
                | "directive.haproxy.option.forwardfor"
                | "directive.haproxy.option.httpchk"
                | "directive.haproxy.option.http-server-close"
                | "directive.haproxy.retry-on.none"
                | "directive.haproxy.retry-on.triggers"
                | "directive.haproxy.retry-on.statuses"
                | "directive.haproxy.retry-on.all"
                | "directive.haproxy.http-check.expect-status"
                | "directive.haproxy.http-check.send"
        )
}

#[expect(
    clippy::too_many_lines,
    reason = "the exhaustive directive fixture registry stays adjacent to its manifest IDs"
)]
pub(super) fn haproxy_probe_source(entry: &DirectiveForm) -> String {
    if entry.id.starts_with("directive.haproxy.section.") {
        return if entry.key == "global" {
            "global\n".into()
        } else {
            format!("{} coverage\n", entry.key)
        };
    }
    if let Some(source) = haproxy_transport_probe(&entry.id) {
        return source;
    }

    match entry.id.as_str() {
        "directive.haproxy.maxconn.global" => {
            format!("global\n  maxconn 4096\n{}", haproxy_tcp_base())
        }
        "directive.haproxy.server.health-options" => haproxy_tcp_base().replace(
            "server primary 127.0.0.1:5432",
            "server primary 127.0.0.1:5432 check inter 2s rise 2 fall 3",
        ),
        "directive.haproxy.server.weighted-tcp" => haproxy_tcp_base().replace(
            "server primary 127.0.0.1:5432",
            "server primary 127.0.0.1:5432 weight 50",
        ),
        "directive.haproxy.server.weighted-http" => haproxy_http_base().replace(
            "server api-1 127.0.0.1:3001",
            "server api-1 127.0.0.1:3001 weight 50",
        ),
        "directive.haproxy.default-server.health-options" => inject_haproxy_defaults(
            "default-server check inter 2s fastinter 1s downinter 5s rise 2 fall 3",
        ),
        "directive.haproxy.default-server.weighted-tcp" => {
            inject_haproxy_defaults("default-server weight 25")
        }
        "directive.haproxy.default-server.weighted-http" => {
            inject_haproxy_http_defaults("default-server weight 25")
        }
        "directive.haproxy.server.passive-health" => haproxy_tcp_base().replace(
            "server primary 127.0.0.1:5432",
            "server primary 127.0.0.1:5432 check observe layer4 error-limit 5 on-error mark-down",
        ),
        "directive.haproxy.default-server.passive-health" => inject_haproxy_defaults(
            "default-server check observe layer4 error-limit 5 on-error mark-down",
        ),
        "directive.haproxy.retries.positive" => {
            haproxy_tcp_base().replace("retries 0", "retries 3")
        }
        "directive.haproxy.retry-on.none" => inject_haproxy_http_defaults("retry-on none"),
        "directive.haproxy.retry-on.triggers" => inject_haproxy_http_defaults(
            "retry-on conn-failure empty-response response-timeout junk-response",
        ),
        "directive.haproxy.retry-on.statuses" => {
            inject_haproxy_http_defaults("retry-on 500 502 504")
        }
        "directive.haproxy.retry-on.all" => inject_haproxy_http_defaults("retry-on all"),
        "directive.haproxy.acl.path-beg" | "directive.haproxy.use-backend" => haproxy_http_base(),
        "directive.haproxy.acl.hdr-host" => haproxy_http_base().replace(
            "acl api path_beg /api",
            "acl api hdr(host) api.example.test",
        ),
        "directive.haproxy.use-backend.host-path-conjunction" => haproxy_http_base()
            .replace(
                "acl api path_beg /api",
                "acl api_host hdr(host) -i api.example.test\n  acl api_path path_beg /api",
            )
            .replace("use_backend api if api", "use_backend api if api_host api_path"),
        "directive.haproxy.use-backend.dynamic-expression" => {
            haproxy_http_base().replace("use_backend api if api", "use_backend api if { path /api }")
        }
        "directive.haproxy.http-request.return" => haproxy_terminal_return_base(),
        "directive.haproxy.http-request.redirect" => haproxy_terminal_redirect_base(),
        "directive.haproxy.http-request.set-header"
        | "directive.haproxy.http-request.del-header"
        | "directive.haproxy.http-response.set-header"
        | "directive.haproxy.http-response.del-header" => haproxy_header_mutation_base(),
        "directive.haproxy.option.redispatch" => inject_haproxy_defaults("option redispatch"),
        "directive.haproxy.option.redispatch-bare-http" => {
            inject_haproxy_http_defaults("option redispatch")
        }
        "directive.haproxy.no-option.redispatch-tcp" => {
            inject_haproxy_defaults("no option redispatch")
        }
        "directive.haproxy.no-option.redispatch-http" => {
            inject_haproxy_http_defaults("no option redispatch")
        }
        "directive.haproxy.option.forwardfor" => inject_haproxy_http_defaults("option forwardfor"),
        "directive.haproxy.no-option.forwardfor-tcp" => {
            inject_haproxy_defaults("no option forwardfor")
        }
        "directive.haproxy.no-option.forwardfor-http" => {
            inject_haproxy_http_defaults("no option forwardfor")
        }
        "directive.haproxy.option.httpchk" => {
            inject_haproxy_http_defaults("option httpchk GET /health")
        }
        "directive.haproxy.option.http-server-close" => {
            inject_haproxy_http_defaults("option http-server-close")
        }
        "directive.haproxy.no-option.httpchk-tcp" => inject_haproxy_defaults("no option httpchk"),
        "directive.haproxy.no-option.httpchk-http" => {
            inject_haproxy_http_defaults("no option httpchk")
        }
        "directive.haproxy.http-check.expect-status" => {
            inject_haproxy_http_defaults("http-check expect status 200")
        }
        "directive.haproxy.http-check.send" => haproxy_http_check_send_probe_source(
            "backend",
            "http-check send meth GET uri /health ver HTTP/1.1 hdr Host app.internal",
        ),
        "directive.haproxy.stats" => inject_haproxy_defaults("stats hide-version"),
        "directive.haproxy.stats-page" => "frontend coverage-stats\n  mode http\n  bind 127.0.0.1:18404\n  stats enable\n  stats uri /stats\n  stats refresh 10s\n  stats admin if LOCALHOST\n".into(),
        "directive.haproxy.log" => inject_haproxy_defaults("log global"),
        "directive.haproxy.log-format" => inject_haproxy_defaults("log-format coverage"),
        "directive.haproxy.error-log-format" => {
            inject_haproxy_defaults("error-log-format coverage")
        }
        "directive.haproxy.unique-id-format" => {
            inject_haproxy_defaults("unique-id-format coverage")
        }
        "directive.haproxy.unique-id-header" => {
            inject_haproxy_defaults("unique-id-header X-Coverage")
        }
        "directive.haproxy.option.logging" => inject_haproxy_defaults("option httplog"),
        _ if entry.disposition == Disposition::Externalized => {
            let argument = if matches!(entry.key.as_str(), "daemon" | "master-worker") {
                ""
            } else {
                " coverage"
            };
            format!("global\n  {}{argument}\n{}", entry.key, haproxy_tcp_base())
        }
        "directive.haproxy.maxconn.proxy-tcp"
        | "directive.haproxy.mode.tcp"
        | "directive.haproxy.bind.tcp"
        | "directive.haproxy.default-backend.tcp"
        | "directive.haproxy.balance.roundrobin-tcp"
        | "directive.haproxy.server.static-ip-tcp"
        | "directive.haproxy.retries.zero-tcp"
        | "directive.haproxy.timeout" => haproxy_tcp_base(),
        "directive.haproxy.maxconn.proxy-http"
        | "directive.haproxy.mode.http"
        | "directive.haproxy.bind.http"
        | "directive.haproxy.default-backend.http"
        | "directive.haproxy.balance.roundrobin-http"
        | "directive.haproxy.server.static-ip-http"
        | "directive.haproxy.retries.zero-http" => haproxy_http_base(),
        id => panic!("HAProxy manifest form has no executable probe: {id}"),
    }
}

pub(super) fn haproxy_transport_probe(id: &str) -> Option<String> {
    match id {
        "directive.haproxy.bind.unix-tcp" => {
            Some(haproxy_tcp_base().replace("bind 127.0.0.1:15432", "bind /run/coverage.sock"))
        }
        "directive.haproxy.bind.unix-http" => {
            Some(haproxy_http_base().replace("bind 127.0.0.1:18080", "bind /run/coverage.sock"))
        }
        "directive.haproxy.balance.leastconn-tcp" => {
            Some(haproxy_tcp_base().replace("balance roundrobin", "balance leastconn"))
        }
        "directive.haproxy.balance.leastconn-http" => Some(
            inject_haproxy_http_defaults("option http-server-close")
                .replace("balance roundrobin", "balance leastconn"),
        ),
        "directive.haproxy.balance.first-tcp" => {
            Some(haproxy_tcp_base().replace("balance roundrobin", "balance first"))
        }
        "directive.haproxy.balance.first-http" => Some(
            inject_haproxy_http_defaults("option http-server-close")
                .replace("balance roundrobin", "balance first"),
        ),
        "directive.haproxy.server.dns-tcp" => Some(haproxy_tcp_base().replace(
            "server primary 127.0.0.1:5432",
            "server primary database.test:5432",
        )),
        "directive.haproxy.server.dns-http" => Some(haproxy_http_base().replace(
            "server api-1 127.0.0.1:3001",
            "server api-1 api.internal.test:3001",
        )),
        "directive.haproxy.server.unix-tcp" => Some(haproxy_tcp_base().replace(
            "server primary 127.0.0.1:5432",
            "server primary /run/database.sock",
        )),
        "directive.haproxy.server.unix-http" => Some(
            haproxy_http_base()
                .replace("server api-1 127.0.0.1:3001", "server api-1 /run/api.sock"),
        ),
        _ => None,
    }
}

pub(super) fn haproxy_tcp_base() -> String {
    "defaults tcp_defaults\n  mode tcp\n  retries 0\n  timeout connect 10s\n  timeout client 5m\n  timeout server 5m\nfrontend database\n  bind 127.0.0.1:15432\n  maxconn 1000\n  default_backend database_pool\nbackend database_pool\n  balance roundrobin\n  server primary 127.0.0.1:5432\n".into()
}

pub(super) fn haproxy_http_base() -> String {
    "defaults web\n  mode http\n  retries 0\n  timeout connect 30s\n  timeout server 30s\nfrontend public\n  bind 127.0.0.1:18080\n  maxconn 100\n  acl api path_beg /api\n  use_backend api if api\n  default_backend fallback\nbackend api\n  balance roundrobin\n  server api-1 127.0.0.1:3001\nbackend fallback\n  balance roundrobin\n  server fallback-1 127.0.0.1:3002\n".into()
}

pub(super) fn haproxy_strict_http_base() -> String {
    "defaults web\n  mode http\n  retries 0\n  timeout connect 30s\n  timeout server 30s\nfrontend public\n  bind 127.0.0.1:18080\n  maxconn 100\n  default_backend app\nbackend app\n  balance roundrobin\n  server app-1 127.0.0.1:3001\n".into()
}

pub(super) fn haproxy_terminal_return_base() -> String {
    "frontend health\n  mode http\n  bind 127.0.0.1:18081\n  maxconn 100\n  http-request return status 200 content-type text/plain string healthy\n".into()
}

pub(super) fn haproxy_terminal_redirect_base() -> String {
    "frontend redirect\n  mode http\n  bind 127.0.0.1:18082\n  maxconn 100\n  http-request redirect location https://example.test/new code 308\n".into()
}

pub(super) fn haproxy_header_mutation_base() -> String {
    "defaults web\n  mode http\n  retries 0\n  timeout connect 30s\n  timeout server 30s\nfrontend public\n  bind 127.0.0.1:18083\n  maxconn 100\n  http-request set-header X-Client-IP %[src]\n  http-request del-header X-Remove\n  default_backend app\nbackend app\n  balance roundrobin\n  http-response set-header X-Frame-Options same-origin\n  http-response del-header X-Powered-By\n  server app1 127.0.0.1:3000\n".into()
}

pub(super) fn haproxy_tcp_listen_base() -> String {
    "defaults tcp_defaults\n  mode tcp\n  retries 0\n  timeout connect 10s\n  timeout client 5m\n  timeout server 5m\nlisten database\n  bind 127.0.0.1:15432\n  maxconn 1000\n  balance roundrobin\n  server primary 127.0.0.1:5432\nbackend database_pool\n  mode tcp\n  balance roundrobin\n  server secondary 127.0.0.1:5433\n".into()
}

pub(super) fn haproxy_http_listen_base() -> String {
    "defaults web\n  mode http\n  retries 0\n  timeout connect 30s\n  timeout server 30s\nlisten public\n  bind 127.0.0.1:18080\n  maxconn 100\n  balance roundrobin\n  server local 127.0.0.1:3001\nbackend database_pool\n  mode http\n  balance roundrobin\n  server primary 127.0.0.1:3002\n".into()
}

pub(super) fn inject_haproxy_defaults(directive: &str) -> String {
    haproxy_tcp_base().replacen(
        "defaults tcp_defaults\n",
        &format!("defaults tcp_defaults\n  {directive}\n"),
        1,
    )
}

pub(super) fn inject_haproxy_http_defaults(directive: &str) -> String {
    haproxy_http_base().replacen(
        "defaults web\n",
        &format!("defaults web\n  {directive}\n"),
        1,
    )
}
