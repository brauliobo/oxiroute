vcl 4.1;

import directors as dir from "/usr/lib/varnish/vmods";
import std;

acl purge_clients {
    "192.0.2.0"/24;
    !"192.0.2.99";
    ("cache.example.test");
}

probe health {
    .request = "GET /ready HTTP/1.1" "Host: cache.example.test" "Connection: close";
    .expected_response = 204;
    .timeout = 1s;
    .interval = 5s;
    .window = 5;
    .threshold = 3;
}

backend disabled none;

backend unix_socket {
    .path = "/run/example/backend.sock";
}

include +glob "conf.d/*.vcl";

sub vcl_init {
    new pool = dir.round_robin();
    pool.add_backend(origin);
    pool.add_backend(unix_socket);
}

sub vcl_recv {
    call classify_request;
    set req.backend_hint = pool.backend();
    if (req.http.Cookie ~ "session=") {
        unset req.http.Cookie;
        return (pass);
    }
    if (req.method == "BAN") {
        ban("req.url == " + req.url);
        return (purge);
    }
    return (hash);
}

sub vcl_hash {
    hash_data(req.url);
    hash_data(req.http.Host);
    return (lookup);
}

sub vcl_hit {
    if (obj.ttl <= 0s) {
        return (miss);
    }
    return (deliver);
}

sub vcl_backend_response {
    set beresp.ttl = 10m;
    set beresp.grace = 1h;
    set beresp.keep = 5m;
    set beresp.uncacheable = true;
    set beresp.do_esi = true;
    set beresp.do_gzip = true;
    return (pass(30s));
}

sub vcl_deliver {
    set resp.http.X-Cache = "edge";
    std.log("delivery classified");
}

sub vcl_synth {
    set resp.http.Location = "/replacement";
    synthetic("redirecting");
    return (synth(302, "Found"));
}

sub inline_native {
    C{ /* retained, never executed */ }C
}
