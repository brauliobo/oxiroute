vcl 4.1;

backend primary {
    .host = "198.51.100.10";
    .port = 8080;
}

backend secondary {
    .host = "198.51.100.11";
    .port = 8080;
}

director pool round-robin {
    { .backend = primary; }
    { .backend = secondary; }
}

sub vcl_recv {
    set req.backend_hint = pool;
    return (hash);
}

sub vcl_hash {
    hash_data(req.url);
    hash_data(req.http.Host);
    return (lookup);
}

sub vcl_backend_response {
    set beresp.ttl = 120s;
    set beresp.grace = 10s;
    set beresp.keep = 300s;
    return (deliver);
}
