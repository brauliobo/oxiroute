vcl 4.1;

backend origin {
    .host = "127.0.0.1";
    .port = 8080;
}

sub vcl_recv {
    set req.backend_hint = origin;
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

sub vcl_deliver {
    set resp.http.X-Cache = "hit";
}
