# Vendored pingora-http

This directory contains the published `pingora-http 0.9.1` source from crates.io
(registry checksum `3f38e9e519d53fab09eb320f7be2cd27ef9e66f755eba3bd84cc68852108d841`)
under its upstream Apache-2.0 license.

OxiRoute's delta is one accessor on `RequestHeader` and `ResponseHeader`, `headers_mut_no_case()`: the header map,
mutably, when no original-case map exists (every parsed request and response), `None` otherwise. 0.9 removed `DerefMut`
from both types, which left `insert_header` / `append_header` as the only way to add a header; with a `HeaderName` in
hand they turn it into text and parse it again (two allocations for a name outside the standard table, for the request id
on every request), which the previous direct map insert did not cost. Callers use the accessor when they can and keep the
case-preserving calls for headers that have a case map. `scripts/test-vendored.sh http` runs the crate's tests.

Upgrade by importing the published crate as its own commit and re-adding the accessor.
