backend origin {
    .host = "192.0.2.10";
    .port = "8080";
    .connect_timeout = 1s;
    .first_byte_timeout = 5s;
    .between_bytes_timeout = 2s;
}

director edge round-robin {
    { .backend = origin; }
}
