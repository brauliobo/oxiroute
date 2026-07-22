# Certificate and ACME specification

## Objective

Provide a Certbot-like certificate lifecycle inside OxiRoute: account registration,
certificate requests, challenge completion, secure storage, installation, monitoring, and
automatic renewal. The implementation is independent and uses the ACME standard; it does
not copy Certbot or plugin code.

OxiRoute implements lifecycle orchestration, not cryptographic primitives. JWS, key, CSR,
X.509, and TLS operations MUST use maintained Rust or system cryptography libraries.

## Certificate sources

### ACME managed

The daemon owns issuance and renewal through an ACME v2 directory.

### Imported files

The daemon reads operator-owned certificate, chain, and private-key paths. It watches their
parent directories and validates replacements before activation.

An optional one-time Certbot lineage configuration import followed by continuous external
source watching reads `live/<name>/fullchain.pem`, `cert.pem`, `chain.pem`, and
`privkey.pem` symlinks. A snapshot is accepted only when all links resolve to one common
numbered archive revision inside that lineage. The importer re-reads metadata before and
after copying to reject transient mixed-link updates. OxiRoute MUST NOT mutate, chmod, or
lock Certbot's lineage, renewal, archive, or account directories.

### Self-signed

For local development and bootstrap only. The daemon generates a leaf key and certificate
for explicit names and validity. Browsers will not trust it automatically. This mode MUST
be visibly labeled and SHOULD default to loopback/private use.

A future local-CA mode requires separate trust-distribution design and is not implied by
self-signed support.

## ACME protocol scope

Initial implementation follows RFC 8555 behavior:

1. Fetch and validate an HTTPS directory.
2. Create or load an account key.
3. Register/update an account with contacts and terms agreement.
4. Acquire and consume replay nonces.
5. Sign protected JWS requests.
6. Create an order for the exact configured DNS identifier set.
7. Select and provision supported challenges.
8. Poll authorization and order state with bounded deadlines and server retry guidance.
9. Generate a new private key unless key reuse is explicitly configured.
10. Create a CSR containing the exact SAN set.
11. Finalize the order and download the certificate chain.
12. Validate, store, activate, and schedule renewal.

`badNonce` receives exactly one ACME-defined retry with a new nonce. Retries for all other
operations are defined per operation: ambiguous non-idempotent account creation, order
creation, revocation, and key rollover are not blindly replayed. Safe polling and retrieval
use bounded exponential backoff with jitter and honor `Retry-After`. Permanent ACME
problem types fail without broad retry loops.

The ACME Renewal Information service SHOULD be used when the directory advertises it.
Otherwise the local renewal policy applies.

## Challenge types

### HTTP-01: first release

- OxiRoute owns an explicit port-80 HTTP listener or an explicitly delegated challenge route.
- `/.well-known/acme-challenge/<token>` is matched before redirects and normal routes.
- Only pending authorizations are challenged; already-valid authorizations are reused.
- Tokens are exact, short-lived records tied to the account, order, authorization, and challenge.
- The handler serves only the required key authorization and no files.
- Requests with malformed or unknown tokens return not found without exposing order state.
- If another process owns port 80, validation fails with an actionable diagnostic rather than changing firewall rules.
- All required challenge material is provisioned before the CA is notified; cleanup always runs after a terminal result or timeout.

### DNS-01: next release

- Required for wildcard identifiers and environments without reachable HTTP port 80.
- Provider integrations run behind a narrow plugin protocol in isolated helper processes.
- Input contains the record name/value and an opaque credential reference, never arbitrary shell commands.
- Plugins return created record identity and cleanup status.
- Propagation checks query authoritative DNS with bounded quorum/deadline policy.
- TXT updates are additive and preserve unrelated or concurrently created values.
- Cleanup removes only the value or provider record ID created by the job.
- Cleanup intent is journaled durably; unresolved cleanup after failure or restart remains visible.
- CNAME/NS delegation behavior is explicit per provider and never guessed.
- Initial provider selection remains small; RFC 2136 is one option only where credentials can be narrowly scoped.

### TLS-ALPN-01

Deferred until dynamic per-name challenge certificate selection is tested across every TLS
backend used by Pingora. It MUST not disrupt ordinary handshakes.

## Domain and authorization policy

- Identifiers MUST be normalized and deduplicated before order creation.
- IP-address certificates are unsupported until both CA and implementation behavior are explicitly tested.
- Wildcards require DNS-01.
- The management authorization policy MUST restrict which DNS suffixes an operator or automation identity may request.
- Imported native config MUST NOT automatically trigger public issuance without explicit certificate-management consent.
- Production and staging directory URLs are distinct configuration values; tests use staging or a local ACME server.
- Directory redirects and every advertised ACME endpoint remain inside configured outbound-origin policy; private/local endpoints require an explicit development allowlist.
- When a directory requires external account binding, registration uses an opaque EAB secret reference or fails before creating an account.

## Key management and storage

Default state layout:

```text
state/
  acme/accounts/<directory-id>/<account-id>/
    account-key.pem
    account.json
  certificates/<certificate-name>/renewal.json
  certificates/<certificate-name>/revisions/<revision>/
    cert.pem
    chain.pem
    fullchain.pem
    privkey.pem
    metadata.json
  certificates/<certificate-name>/current
```

- State root and account directories: owner-only access.
- Private/account key files: mode `0600`; certificate files no broader than `0644`.
- Managed directories MUST be owned by the daemon identity; startup rejects unsafe owners, modes, links, and hard links.
- Secret files use exclusive, no-follow creation.
- Writes use unique same-directory temporary files, sync, rename, and directory sync on one local filesystem.
- `current` is one atomically replaced relative symlink to a complete revision directory, yielding stable paths such as `current/fullchain.pem`.
- Certificate names are strict path-safe slugs or opaque IDs.
- Symlink handling MUST reject paths escaping the configured state root for managed material.
- Backups and diagnostics MUST not copy private keys into general logs or support bundles.
- Hardware or external key stores are a future `KeyProvider`; lifecycle code must not assume keys are exportable.
- A process-lifetime OS lock protects the state root; per-certificate jobs also coalesce in process.
- Shared/network filesystems and multiple daemons writing one state root are unsupported in the first release.

`account.json` persists the exact directory URL, account URL/JWS key ID, status, contacts,
terms record, and public-key fingerprint. `directory-id` is a collision-resistant hash of
the canonical full URL; staging and production accounts are never shared implicitly.

`renewal.json` persists exact identifiers, directory and account references, authenticator
and non-secret options, opaque credential references, key policy, last result, and
scheduler/renewal-information state. It is certificate-level state rather than revision
metadata.

Supported key policies initially:

- ECDSA P-256 default where the TLS backend supports it.
- RSA 2048+ optional for compatibility.
- New leaf key per renewal by default.
- Separate ACME account and leaf keys.

## Validation before activation

The candidate MUST pass all checks:

- Private key matches leaf certificate.
- Leaf and chain parse without trailing unrelated private material.
- Returned SANs exactly equal the normalized requested identifier set and cover every configured listener reference using standard hostname rules.
- Current time is within validity with configurable clock-skew allowance.
- Basic constraints and key usages permit TLS server use.
- Chain ordering is coherent and accepted by the selected TLS backend.
- Signature/key type is supported by the active build.
- Certificate is not known revoked when a configured revocation source can be checked.

Failure leaves the previous revision active and records a redacted diagnostic.

## Renewal policy

- The scheduler evaluates every managed certificate at startup and at least every 12 hours.
- Use the CA-provided suggested renewal window when available; persist the selected stable random time and server retry guidance.
- Otherwise renew when remaining lifetime is at most one third of original lifetime, or one half for certificates whose original lifetime is shorter than 10 days.
- Select and persist a stable random time within the renewal window to avoid synchronized fleets.
- An operator may request early renewal, but duplicate concurrent jobs coalesce by certificate name.
- Repeated transient failures back off with jitter while ensuring attempts become more frequent near expiry.
- Ordinary authorization failures continue bounded scheduled attempts; only explicit operator pause or demonstrably invalid local configuration suspends attempts.
- Emit escalating warnings at 30, 14, 7, 3, and 1 day when no valid replacement is active.
- Expiry MUST NOT silently replace a certificate with self-signed material.

## Zero-downtime installation

Challenge authenticators only provision and clean authorization material. The OxiRoute
activator validates and publishes Pingora TLS generations. The first release has one
built-in HTTP-01 authenticator and one internal activator; it does not discover general
web-server installers or maintain server-configuration checkpoints.

1. Complete issuance into a private revision directory.
2. Validate certificate, key, chain, names, and TLS backend loading.
3. Prepare new listener TLS contexts without changing active routing.
4. Atomically move the managed `current` pointer.
5. Atomically publish the new runtime certificate map or generation.
6. Keep prior contexts alive for handshakes/connections already referencing them.
7. Emit revision and expiry events.

If runtime preparation fails after files are stored, disk certificate revision and active
certificate revision differ visibly; the old active certificate remains.

## Revocation and deletion

- The API MAY request ACME revocation with explicit confirmation and reason.
- Revocation is never an automatic consequence of removing a listener.
- Deleting a managed certificate first requires no active TLS profile references.
- Account key rollover and account deactivation are administrative operations with audit records.
- Old private-key revisions use configurable retention and secure deletion where the filesystem can provide meaningful guarantees.

## API and UI behavior

Inventory fields:

- Name, source, domains, issuer, serial fingerprint, key type, not-before/not-after.
- Disk and active revision, state, next scheduled action, last success, and last redacted error.
- ACME directory and challenge type, but never account URL tokens, private keys, or DNS credentials.

Actions:

- Validate imported files.
- Issue using a staging or production directory.
- Renew now, pause/resume automatic renewal, revoke, and delete when unreferenced.
- Display challenge progress and cleanup failures.

Production issuance requires an explicit confirmation in the UI until the certificate
configuration has completed a successful staging or dry validation path.

## Observability

Metrics include:

- Certificate seconds until expiry and active revision age.
- ACME jobs by operation/result/directory class.
- Challenge attempts and duration by type/result.
- Renewal scheduler due/overdue counts.
- Activation and rollback results.

Metric labels MUST not include unbounded token, order URL, serial, or full domain-set data.
Audit events identify the certificate by configured stable name.

## Failure and recovery

- Restart resumes scheduler/account state but abandons in-flight issuance and creates a fresh order on the next attempt.
- Incomplete temporary revisions are ignored and cleaned after an age threshold.
- Account key loss is a hard diagnostic; no new account is silently substituted for the same configured identity.
- System clock problems block issuance/activation with a specific diagnostic.
- Rate-limit responses expose the retry time and do not spin.

## Hooks and extensions

Arbitrary pre/post/deploy shell hooks are excluded from the first release. Successful
runtime publication emits `certificate.activated`. A future hook runner requires
allowlisted no-shell executables, an unprivileged identity, input/output/time limits,
revision idempotency, at-least-once semantics, and failures that do not roll back a valid
active certificate.

## Licensing and provenance

Certbot and its ACME library are Apache-2.0; parts of its nginx plugin include MIT-licensed
nginxparser code. This specification uses architectural lessons and protocol behavior.
Any future source reuse requires file-level license and attribution review, and no upstream
license grants use of its trademarks.

## Test requirements

- Local ACME server or CA staging integration; production endpoints are never used in CI.
- New account, existing account, terms failure, external-account-binding fixture when supported.
- HTTP-01 success, wrong token, unreachable listener, redirect coexistence, and cleanup.
- DNS-01 plugin success, propagation timeout, credential error, and cleanup failure.
- Nonce retry, order invalidation, polling timeout, rate limit, malformed chain, and key mismatch.
- Renewal-window and stable-jitter property tests with a fake clock.
- Crash/failure injection before and after every durable rename and runtime activation.
- Existing traffic continues through certificate rotation.
- Imported Certbot symlink changes and transient mixed revisions are handled without modifying its files.
- API and logs never contain private or account key bytes.
- State-root lock, owner/mode, no-follow, hard-link, unsafe-name, and non-local-filesystem failures.

Implementation begins with failing state-machine and storage tests before any live ACME
request code.
