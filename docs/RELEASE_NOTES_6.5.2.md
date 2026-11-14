# OxiRoute 6.5.2

Released 2026-11-14.

OxiRoute 6.5.2 is a bug-fix release: an operator route for `/.well-known/acme-challenge/` is now reached, so a webroot
served for Certbot or another ACME client keeps renewing certificates behind OxiRoute. Nothing else changes from 6.5.1.
The release retains conservative compatibility claims.

## Fixed

- **Webroot ACME HTTP-01 routes behind OxiRoute.** The built-in HTTP-01 challenge store answered every request under
  `/.well-known/acme-challenge/` itself, with an empty `404` for tokens it did not hold and for methods other than `GET`
  and `HEAD`, before route selection. A route for that prefix, such as the static webroot that
  `oxiroute import nginx` lowers `location ^~ /.well-known/acme-challenge/ { root ...; try_files $uri =404; }` to, was
  therefore never reached, and challenges written to that webroot failed validation. The store now takes over a request
  only when it holds the token (`GET` or `HEAD`, a valid unexpired token); every other request under the prefix is routed
  like any other path, and the default `404` applies when no route matches. The fix covers the HTTP/1.1 and HTTP/2 fast
  lanes and the classic path, and the lanes keep serving every path the store does not hold.

## Upgrade notes

- No migration is needed. Managed ACME HTTP-01 issuance behaves as before; a request for a held token is answered as
  before.
- An unknown token under the prefix with no operator route still receives `404`, now the router's response instead of
  the challenge store's empty one.
- Documentation: [ACME specification](ACME_SPEC.md) and [Configuration specification](CONFIG_SPEC.md).

## Build

- The release assets are built by the same pipeline as 6.5.1.
