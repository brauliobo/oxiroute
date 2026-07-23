sub audit_request {
    set req.http.X-Audit = "classified";
}

sub vcl_recv {
    if (client.ip ~ purge_clients) {
        return (purge);
    }
}
