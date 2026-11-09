#!/usr/bin/env python3
"""Render an OxiRoute Lua configuration for the R2 data-path harness.

Usage: gen_config.py OUT --proxy-port P --upstream-port U [--servers N] [--echo-port E]
                         [--run-dir DIR] [--feature NAME]...

Every feature is an independent A/B switch on top of the minimal reverse-proxy configuration:
  access_log timeouts retry queue capacity passive breaker outlier gzip cache reqbuf respbuf
  forward forwarded dns dns_startup retry_buffered fixed static tls tls_cache tls_tickets tls_rsa upstream_h2 l4 stats nginx_host lc wrr
"""

import argparse
import json


def lua(value, indent=0):
    pad = "  " * (indent + 1)
    end = "  " * indent
    if value is None:
        return "nil"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return json.dumps(value)
    if isinstance(value, str):
        return json.dumps(value)
    if isinstance(value, list):
        items = "".join(f"{pad}{lua(item, indent + 1)},\n" for item in value)
        return "{\n" + items + end + "}"
    items = "".join(
        f"{pad}{key} = {lua(item, indent + 1)},\n" for key, item in value.items() if item is not None
    )
    return "{\n" + items + end + "}"


def algorithm(args, on):
    if on("lc"):
        return "least_connections"
    if on("wrr"):
        return {"type": "weighted_round_robin", "weights": [1 + index % 5 for index in range(args.servers)]}
    return "round_robin"


def build(args):
    features = set(args.feature)
    on = features.__contains__
    proxy_policy = {
        "upstream_host": {"type": "preserve_incoming"},
        "retry": {
            "max_retries": 0,
            "triggers": ["connect_failure", "connect_timeout", "refused_stream"],
            "method_safety": "get_head",
            "body_safety": "empty",
        },
    }
    if on("nginx_host"):
        proxy_policy["upstream_host"] = {"type": "endpoint"}
    if on("forwarded"):
        # What an nginx `proxy_set_header` block does: forwarded headers, a host rewrite and a response header.
        proxy_policy["upstream_host"] = {"type": "endpoint"}
        proxy_policy["request_headers"] = [
            {"operation": "set", "name": "x-forwarded-for", "value": {"type": "appended_x_forwarded_for", "max_bytes": 256}},
            {"operation": "set", "name": "x-forwarded-proto", "value": {"type": "downstream_scheme"}},
            {"operation": "set", "name": "x-forwarded-host", "value": {"type": "incoming_authority"}},
            {"operation": "set", "name": "x-real-ip", "value": {"type": "client_ip"}},
        ]
        proxy_policy["response_headers"] = [
            {"operation": "set", "name": "x-served-by", "value": "bench", "always": True},
            {"operation": "remove", "name": "x-powered-by"},
        ]
    if on("timeouts"):
        proxy_policy["timeouts"] = {"connect_ms": 2000, "header_ms": 10000, "idle_ms": 10000, "total_ms": 30000}
    if on("retry_buffered"):
        proxy_policy["retry"] = {
            "max_retries": 1,
            "triggers": ["connect_failure", "connect_timeout", "refused_stream"],
            "method_safety": "all",
            "body_safety": "buffered",
        }
    if on("retry"):
        proxy_policy["retry"] = {
            "max_retries": 2,
            "triggers": ["connect_failure", "connect_timeout", "refused_stream"],
            "response_statuses": [502, 503],
            "method_safety": "get_head",
            "body_safety": "empty",
            "budget": {"per_second": 100000, "ratio_percent": 50},
        }
    if on("queue"):
        proxy_policy["queue_headers"] = True
        proxy_policy["queue_class"] = "high"
    if on("cache"):
        proxy_policy["cache"] = {"store": "mem"}

    route_policy = {}
    if on("reqbuf"):
        route_policy.update({"request_buffering": True, "max_request_body_bytes": 10485760})
    if on("respbuf"):
        route_policy.update({"response_buffering": True, "max_request_body_bytes": 10485760})

    routes = []
    if on("fixed"):
        routes.append({
            "path": {"kind": "segment_prefix", "value": "/fixed"},
            "methods": [],
            "action": {"type": "fixed_response", "status": 200, "body": "ok\n"},
        })
    if on("static"):
        routes.append({
            "path": {"kind": "segment_prefix", "value": "/static"},
            "methods": ["GET", "HEAD"],
            "action": {"type": "static_files", "root_directory": f"{args.run_dir}/www"},
        })
    proxy_route = {
        "path": {"kind": "segment_prefix", "value": "/"},
        "methods": [],
        "action": {"type": "proxy", "upstream_pool": "origin", "policy": proxy_policy},
    }
    if route_policy:
        proxy_route["policy"] = route_policy
    routes.append(proxy_route)

    service = {
        "name": "bench",
        "routes": routes,
        "upstream_io_timeout_ms": 30000,
        "max_request_body_bytes": 10485760,
    }
    if on("access_log"):
        service["access_log"] = {"type": "file", "path": f"{args.run_dir}/logs/access.log"}
    if on("gzip"):
        service["gzip"] = {"level": 1, "content_types": ["text/html"], "min_length_bytes": 20, "min_http_version": "1.1"}

    servers = []
    for index in range(args.servers):
        address = f"127.0.0.{index + 1}:{args.upstream_port}" if args.servers > 2 else f"127.0.0.1:{args.upstream_port + index}"
        server = {"name": f"origin-{index + 1}", "endpoint": {"type": "socket", "address": address}}
        if on("capacity"):
            server["max_connections"] = 100000
        if on("dns") or on("dns_startup"):
            server["endpoint"] = {"type": "dns", "host": "localhost", "port": args.upstream_port + index}
            server["dns_resolution"] = "startup" if on("dns_startup") else "on_connect"
        servers.append(server)
    pool = {
        "name": "origin",
        "servers": servers,
        "algorithm": algorithm(args, on),
        "http_versions": {"min": "2", "max": "2"} if on("upstream_h2") else {"min": "1.1", "max": "1.1"},
    }
    if on("upstream_h2"):
        pool["tls"] = {"server_name": "origin.example.com", "ca_certificate_path": f"{args.run_dir}/origin.pem"}
    if on("queue"):
        pool["queue_timeout_ms"] = 1000
        pool["queue"] = {
            "classes": [{"name": "high", "weight": 3}, {"name": "low", "weight": 1}],
            "discipline": "weighted",
        }
    passive = on("passive") or on("breaker") or on("outlier")
    if passive:
        policy = {"observe": "layer7", "on_error": "count", "error_limit": 3}
        if on("breaker"):
            policy["circuit_breaker"] = {
                "window_ms": 30000, "min_requests": 20, "failure_ratio_percent": 50,
                "open_ms": 10000, "half_open_max": 1, "half_open_successes": 3,
            }
        if on("outlier"):
            policy["outlier_detection"] = {
                "factor_percent": 300, "min_samples": 50, "interval_ms": 10000, "max_ejected_percent": 30,
            }
        pool["passive_health"] = policy

    listener = {
        "name": "bench",
        "bind": {"type": "socket", "address": f"127.0.0.1:{args.proxy_port}"},
        "protocol": "http",
        "service": "bench",
    }
    config = {
        "version": 1,
        "listeners": [listener],
        "cache_stores": [{"name": "mem", "type": "memory"}] if on("cache") else [],
        "upstream_pools": [pool],
        "http_services": [service],
        "forward_proxy_services": [],
        "rtmp_services": [],
        "l4_services": [],
    }
    if on("tls") or on("tls_cache") or on("tls_tickets") or on("tls_rsa"):
        key_type = "rsa_2048" if on("tls_rsa") else "ecdsa_p256"
        config["certificates"] = [{
            "name": "bench-cert",
            "dns_names": ["bench.example.com"],
            "source": {"type": "self_signed_development", "key_type": key_type},
        }]
        tls_policy = {"session_tickets": on("tls_tickets")}
        if on("tls_cache"):
            tls_policy["session_cache"] = {"name": "bench-sessions", "size_bytes": 4194304}
        config["tls_profiles"] = [{
            "name": "bench-tls",
            "certificates": ["bench-cert"],
            "default_certificate": "bench-cert",
            "min_version": "1.2",
            "alpn": ["h2", "http/1.1"],
            "policy": tls_policy,
        }]
        listener["tls_profile"] = "bench-tls"
    for index in range(1, args.listeners):
        config["listeners"].append(
            dict(listener, name=f"bench{index}", bind={"type": "socket", "address": f"127.0.0.1:{args.proxy_port + 10 + index}"}))
    if args.workers:
        config["listener_workers"] = args.workers
    if on("l4"):
        config["upstream_pools"].append({
            "name": "echo",
            "servers": [{"name": "echo-1", "endpoint": {"type": "socket", "address": f"127.0.0.1:{args.echo_port}"}}],
        })
        config["l4_services"].append({"name": "echo", "upstream_pool": "echo"})
        config["listeners"].append({
            "name": "echo",
            "bind": {"type": "socket", "address": f"127.0.0.1:{args.proxy_port + 1}"},
            "protocol": "tcp",
            "service": "echo",
        })
    if on("forward"):
        config["forward_proxy_services"].append({
            "name": "fwd", "tls_required": False, "destination_policy": {"deny_private": False},
        })
        config["listeners"].append({
            "name": "fwd",
            "bind": {"type": "socket", "address": f"127.0.0.1:{args.proxy_port + 3}"},
            "protocol": "forward_http1",
            "service": "fwd",
        })
    if on("stats"):
        config["stats"] = {"binds": [f"127.0.0.1:{args.proxy_port + 2}"]}
    return config


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("out")
    parser.add_argument("--proxy-port", type=int, required=True)
    parser.add_argument("--upstream-port", type=int, required=True)
    parser.add_argument("--echo-port", type=int, default=0)
    parser.add_argument("--servers", type=int, default=1)
    parser.add_argument("--listeners", type=int, default=1)
    parser.add_argument("--workers", type=int, default=0, help="listener_workers (0 leaves the setting out)")
    parser.add_argument("--run-dir", default="/tmp")
    parser.add_argument("--feature", action="append", default=[])
    args = parser.parse_args()
    with open(args.out, "w", encoding="utf-8", newline="\n") as handle:
        handle.write("return " + lua(build(args)) + "\n")


if __name__ == "__main__":
    main()
