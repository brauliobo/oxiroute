vcl 4.1;

probe shared {
    .url = "/ready";
    .expected_response = 204;
    .timeout = 1s;
    .interval = 5s;
    .window = 1;
    .threshold = 1;
    .initial = 1;
}

probe default {
    .timeout = 2s;
    .interval = 10s;
    .window = 1;
    .threshold = 1;
}

backend origin {
    .host = "127.0.0.1";
    .port = 8080;
    .probe = shared;
}

backend fallback {
    .host = "127.0.0.2";
    .port = 8080;
}

backend inline {
    .host = "127.0.0.3";
    .port = 8080;
    .probe = {
        .url = "/inline-ready";
        .timeout = 1s;
        .interval = 5s;
        .window = 1;
        .threshold = 1;
    }
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
