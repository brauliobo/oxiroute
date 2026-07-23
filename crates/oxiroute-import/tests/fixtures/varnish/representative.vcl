vcl 4.1;

include "shared-backends.vcl";
import std;

sub vcl_recv {
    if (req.http.Authorization) {
        return (pass);
    }
    if (req.http.Cookie ~ "session=") {
        unset req.http.Cookie;
        return (pass);
    }
    if (req.http.Upgrade == "websocket") {
        return (pipe);
    }
    if (req.method == "PURGE") {
        ban("req.url == " + req.url);
        return (purge);
    }
    set req.backend_hint = edge;
    return (hash);
}

sub vcl_hash {
    hash_data(req.url);
    hash_data(req.http.Host);
    return (lookup);
}

sub vcl_backend_response {
    set beresp.ttl = 10m;
    set beresp.grace = 1h;
    set beresp.keep = 5m;
    return (deliver);
}

sub vcl_deliver {
    set resp.http.X-Cache = "edge";
    unset resp.http.Server;
    std.log("delivery audited");
    custom_audit(req.url);
}

sub vcl_synth {
    set resp.http.Location = "/replacement";
    set resp.http.Set-Cookie = "notice=1";
    synthetic("redirecting");
    return (synth(302, "Found"));
}
