# OxiRoute 6.5.0

Unreleased.

## TLS

- **The rustls backend accepts every TLS policy option.** `policy.cipher_list` and `policy.dh_parameters_path` were
  refused with `backend = "rustls"`; they are accepted now, so an nginx import or a certbot-style profile can switch
  backends by changing one line. `cipher_list` keeps its OpenSSL meaning: the string is evaluated by OpenSSL and the TLS
  1.2 suites the rustls backend implements (ECDHE with AES-GCM or ChaCha20-Poly1305) are offered in that order. Suites it
  does not implement (DHE, CBC, RSA key exchange) are named once in the log; a profile whose list leaves no TLS 1.2 suite
  fails to start unless `min_version` is `1.3`. `dh_parameters_path` is accepted and ignored with a warning (no
  finite-field DHE).
- **Session resumption behaves the same on both backends.** A full TLS 1.3 handshake sends one session ticket (the
  OpenSSL backend sent two, the rustls backend four; each further ticket costs a full handshake of server work), and none
  when the profile has neither tickets nor a cache. The default session timeout of the rustls backend is 7200 s as on
  OpenSSL (it was 300 s), and the ticket lifetime hint carries the `session_timeout_seconds` of the profile on both.
  The rustls backend keeps one session state per profile, so a session resumes on every listener of the profile.
- **TLS 1.2 cipher names in the access-log digest and in variables are the OpenSSL names on the rustls backend**
  (`ECDHE-ECDSA-AES128-GCM-SHA256` instead of `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256`), as on the OpenSSL backend.
- **Fixed: a proxied WebSocket held about 117 KB more memory on the rustls backend than on the OpenSSL backend.** Every
  read zero-filled the whole spare buffer of the caller. Idle TLS WebSocket connections now hold 38 to 40 KB each on the
  rustls backend (170 KB before; 53 KB on OpenSSL).
- The rustls backend no longer zero-fills the read buffer on every read: -1% to -2% instructions per keep-alive,
  HTTP/2 and static request over TLS, and lower cycles.
- SNI lookups no longer allocate for lower-case names.

## Benchmarks

- `benchmarks/scripts/vs_nginx_haproxy.py` compares the TLS rows: TLS 1.2 ECDHE handshakes, resumption by ticket and by
  session id, client certificates, `VS_TLS_VARIANTS` for OxiRoute columns (OpenSSL, rustls, inline or offloaded
  handshakes), and `vs_tls_tables.py` renders them. `r2tool` can present a client certificate; the R2 `--mem` mode runs on
  hosts without the `tls` kernel module.
