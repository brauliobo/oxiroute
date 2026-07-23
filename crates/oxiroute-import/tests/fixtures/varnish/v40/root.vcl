vcl 4.0;

include "legacy-backends.vcl";

director legacy_pool round-robin {
    { .backend = legacy_a; }
    { .backend = legacy_b; }
}

sub vcl_recv {
    set req.backend_hint = legacy_pool;
    return (hash);
}

sub vcl_hash {
    hash_data(req.url);
    return (lookup);
}
