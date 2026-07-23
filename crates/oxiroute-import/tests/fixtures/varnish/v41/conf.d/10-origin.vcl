backend origin {
    .host = "192.0.2.10";
    .port = "8080";
    .probe = health;
    .connect_timeout = 1s;
    .first_byte_timeout = 5s;
}

sub classify_request {
    call audit_request;
    if (req.http.Authorization) {
        return (pass);
    }
}

sub vcl_recv {
    if (req.http.X-Bypass) {
        return (pass);
    }
}
