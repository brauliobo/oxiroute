# Certificate and ACME specification

## Objective

The future managed-certificate target is a Certbot-like lifecycle inside OxiRoute: account
registration, certificate requests, challenge completion, secure storage, installation,
monitoring, and automatic renewal. That implementation will be independent and use the ACME
standard; it will not copy Certbot or plugin code.

OxiRoute will implement managed lifecycle orchestration, not cryptographic primitives. JWS, key,
CSR, X.509, and TLS operations MUST use maintained Rust or system cryptography libraries.

Current implementation boundary: strict direct-file startup loading and descriptor-relative Certbot
lineage snapshots prepare immutable certificate generations. One process-lifetime Certbot watcher
supervisor combines bounded filesystem-event coalescing with periodic full rescans, validates a
complete lineage candidate, and atomically publishes each identity independently. Managed ACME now
has bounded owner-only state, injected and production HTTPS transports, account/order/challenge
orchestration, HTTP-01 routing, DNS-01 orchestration, wildcard support, authenticated renewal, a due-check
supervisor, redacted monitoring,
and UI configuration. A first-start managed identity receives an in-memory, one-day self-signed
bootstrap generation marked immediately due; bootstrap material is never persisted as ACME state.
A TLS callback snapshots the active generation for each new handshake; existing connections retain
their selected generation and downstream session resumption is disabled.

## Implemented managed ACME slice

The current implementation covers bounded HTTP-01 and DNS-01 issuance:

- ACME v2 directory discovery, configured-origin enforcement, account registration, persisted account
  state, JWS requests, bounded response/request sizes, and one `badNonce` retry.
- Exact DNS identifier normalization and order matching, strict account/order/authorization state
  handling, bounded polling with deterministic jitter, and integer or HTTP-date `Retry-After` support.
- Owner-only state roots with a process lock, atomic revision pointers, zeroized private-key buffers,
  redacted job records, and atomic certificate material publication.
- In-memory one-day bootstrap certificates, HTTP-01 challenge leases with ownership-safe cleanup,
  exact SAN/key/chain/TLS validation, deterministic renewal scheduling, persisted retry state, and
  immediate or supervised renewal operations.
- DNS-01 wildcard authorization parsing, SHA-256 TXT values, statically linked exact-name provider
  registration, bounded credentials and cancellation, provider-controlled propagation checks, and
  ownership-safe exact-record cleanup.
- Authenticated TLS inventory and renewal API responses with bounded categorical outcomes. Existing
  certificate generations remain active when validation or publication fails.
- Authenticated ACME certificate revocation, RFC 8555 account-key rollover, cooperative job
  cancellation, pause/resume controls, correlation-linked durable jobs, and redacted action outcomes.
- Configurable revision retention with automatic garbage collection that always preserves the active
  pointer and newest retained revisions. State deletion is guarded by active TLS-profile usage.

The implementation does not yet fetch or apply ACME Renewal Information responses, support TLS-ALPN-01,
or provide durable DNS cleanup journaling. Those remain future contract work and must not be inferred
from the current status fields.

## Certificate sources

### ACME managed

The daemon owns issuance and renewal through an ACME v2 directory. Managed certificates support
HTTP-01 and DNS-01; wildcard identifiers require DNS-01. IP identifiers and TLS-ALPN-01 remain
unsupported. Explicit terms agreement and a configured DNS-suffix policy are required.

### Imported files

The daemon reads operator-owned certificate, chain, and private-key paths. Continuous replacement
watching is currently implemented only for configured Certbot lineages.

Certbot lineage startup loading and continuous reconciliation are implemented. They read the configured live
`fullchain.pem`, `cert.pem`, `chain.pem`, and `privkey.pem` symlinks. A snapshot is accepted only
when all links resolve to one common numbered archive revision inside the configured archive.
Archive artifacts are opened relative to a pinned directory descriptor with no-follow semantics,
read twice within bounds, and checked for exact cert/chain/fullchain shape and a secure private-key
mode. Parent and canonical lineage directories are watched non-recursively and rebuilt after each
rescan so directory replacement recovers; a periodic rescan remains the authoritative backstop.
Invalid or transient mixed candidates retain the previous active generation. OxiRoute MUST NOT
mutate, chmod, or lock Certbot's lineage, renewal, archive, or account directories.

### Self-signed

This mode is for local development and managed-ACME bootstrap only. The daemon generates a leaf key
and certificate for explicit names and validity. Browsers will not trust it automatically. Managed
bootstrap material is in-memory only and is replaced by the first validated ACME revision. The mode
MUST be visibly labeled and SHOULD default to loopback/private use.

A future local-CA mode requires separate trust-distribution design and is not implied by
self-signed support.

## ACME protocol scope

The first managed implementation follows RFC 8555 behavior:

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

Revocation is explicit and never follows certificate removal automatically. Account key rollover uses
the directory `keyChange` endpoint and a nested JWS: the inner payload is signed by the replacement key
and the outer request is signed by the current account key. A replacement key is installed locally only
after the CA accepts the request.

The ACME Renewal Information service SHOULD be used when the directory advertises it.
Otherwise the local renewal policy applies.

## Challenge types

### HTTP-01: implemented first release

- OxiRoute will own an explicit port-80 HTTP listener or an explicitly delegated challenge route.
- `/.well-known/acme-challenge/<token>` is matched before redirects and normal routes.
- Only pending authorizations are challenged; already-valid authorizations are reused.
- Tokens are exact, short-lived records tied to the account, order, authorization, and challenge.
- The handler serves only the required key authorization and no files.
- Requests with malformed or unknown tokens return not found without exposing order state.
- If another process owns port 80, validation fails with an actionable diagnostic rather than changing firewall rules.
- All required challenge material is provisioned before the CA is notified; cleanup always runs after a terminal result or timeout.

### DNS-01: implemented

- Required for wildcard identifiers and environments without reachable HTTP port 80.
- Providers implement a narrow in-process contract and must be statically linked and exactly
  allowlisted. Dynamic loading and shell hooks are rejected.
- Provider calls receive only bounded credentials, the exact record name/value, and a deadline with
  cooperative cancellation. Provider errors are categorical and redacted.
- Providers return an opaque record identity, verify propagation within the same operation budget, and
  remove only that exact record during cleanup.
- The orchestrator validates every returned record against the requested challenge before notifying
  the CA. Cleanup runs after propagation, notification, polling, timeout, or ACME failure.
- Provider-specific CNAME/NS delegation and authoritative DNS policy remain provider responsibilities;
  the orchestrator never guesses them.

### TLS-ALPN-01

Deferred until dynamic per-name challenge certificate selection is tested across every TLS
backend used by Pingora. It MUST not disrupt ordinary handshakes.

## Planned domain and authorization policy

- Identifiers MUST be normalized and deduplicated before order creation.
- IP-address certificates are unsupported until both CA and implementation behavior are explicitly tested.
- Wildcards require DNS-01.
- The management authorization policy MUST restrict which DNS suffixes an operator or automation identity may request.
- Imported native config MUST NOT automatically trigger public issuance without explicit certificate-management consent.
- Production and staging directory URLs are distinct configuration values; tests use staging or a local ACME server.
- Directory redirects and every advertised ACME endpoint remain inside configured outbound-origin policy; private/local endpoints require an explicit development allowlist.
- When a directory requires external account binding, registration uses an opaque EAB secret reference or fails before creating an account.

## Planned key management and storage

Planned default state layout:

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
- Revision garbage collection keeps at least the configured newest revision count and age window,
  always retaining the current pointer. Account and revision deletion requires an operator action and
  an active-generation check that no TLS profile references the certificate.
- Shared/network filesystems and multiple daemons writing one state root are unsupported in the first release.

`account.json` will persist the exact directory URL, account URL/JWS key ID, status, contacts,
terms record, and public-key fingerprint. `directory-id` is a collision-resistant hash of
the canonical full URL; staging and production accounts are never shared implicitly.

`renewal.json` will persist exact identifiers, directory and account references, authenticator
and non-secret options, opaque credential references, key policy, last result, and
scheduler/renewal-information state. It is certificate-level state rather than revision
metadata.

Planned initial key policies:

- ECDSA P-256 default where the TLS backend supports it.
- RSA 2048+ optional for compatibility.
- New leaf key per renewal by default.
- Separate ACME account and leaf keys.

## Current validation before activation

The current managed candidate MUST pass these checks:

- Private key matches leaf certificate.
- Leaf and chain parse without trailing unrelated private material.
- Returned DNS SANs exactly equal the normalized requested identifier set.
- Current time is within certificate validity.
- Basic constraints and key usages permit TLS server use.
- Chain ordering is coherent and accepted by the selected TLS backend.
- Signature/key type is supported by the active build.

Failure leaves the previous revision active and records a redacted diagnostic.

The current managed path additionally requires the returned DNS SAN set to equal the configured set,
rejects non-DNS SAN entries and oversized chains, records issuer and serial fingerprints, and relies
on the existing TLS backend parser for key matching, usage, validity, and chain checks. Revocation
status checks and configurable clock-skew policy are not yet implemented.

## Current renewal policy

- The scheduler evaluates every managed certificate immediately at startup and then at its next
  persisted action, capped at a 12-hour scan interval.
- The local policy renews when remaining lifetime enters the one-third window, or the one-half window
  for certificates shorter than 10 days, using a stable deterministic time within that window.
- Failed jobs persist a retry time and attempt number with deterministic jitter, doubling from five
  minutes up to twelve hours. A successful publication resets the retry state.
- An operator may request early renewal, and duplicate concurrent jobs are rejected by certificate
  name. Expiry never silently replaces a certificate with self-signed material.
- Operators may pause/resume automatic renewal or cancel an in-flight polling job. Cancellation and
  pause cleanup paths retain the active certificate and run DNS cleanup independently of the cancelled
  operation.
- The directory's Renewal Information endpoint is recorded when advertised but is not yet fetched or
  applied to scheduling; the local policy remains authoritative until that work is implemented.
- Warning thresholds and escalating expiry events remain future work.

## Current zero-downtime installation

Challenge authenticators only provision and clean authorization material. The OxiRoute activator
validates and publishes Pingora TLS generations. The current release has HTTP-01 routing and a
statically registered DNS-01 provider seam; it does not discover general web-server installers or
maintain server-configuration checkpoints. The existing Certbot watcher uses the same
generation-publication seam but is not an ACME authenticator or scheduler.

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

- The API MAY request ACME revocation with explicit confirmation and an optional RFC 5280 reason.
- Revocation is never an automatic consequence of removing a listener.
- Deleting managed state first requires no active TLS profile references. The current in-memory
  generation remains available until a configuration reload; deletion does not implicitly edit the
  canonical configuration.
- Account key rollover is an administrative operation with an audit/correlation record.
- Old private-key revisions use configurable count/age retention and secure file removal within the
  owner-locked state root.

## Current API and UI behavior

Inventory fields:

- Name, source, domains, issuer, serial fingerprint, key type, not-before/not-after.
- Disk and active revision, state, next scheduled action, last success, and last redacted error.
- ACME directory, challenge type, and non-secret provider name, but never account URL tokens, private
  keys, credential contents, or DNS TXT values.

Actions:

- Validate imported files.
- Issue using a staging or production directory.
- Renew now, pause/resume automatic renewal, revoke, and delete when unreferenced.
- Rollover the account key, cancel active jobs, and display challenge progress and cleanup failures.

The current API exposes the configured directory, challenge and non-secret provider name, key type,
suffix policy, disk/active revisions, expiry, next action, job status, retry attempt, last success,
  redacted outcome/error, job ID/correlation, pause state, and lineage retention policy. Production
confirmation and staging/dry-validation gating remain operator policy rather than an implicit API
fallback.

## Observability

Implemented metrics include:

- Certificate presence and seconds until expiry.
- Renewal due state and the last bounded job result or error code.

The current bounded monitoring snapshot intentionally exposes only stable revision, expiry, scheduler,
and categorical outcome/error fields. Retry counters and job phases are available through the
authenticated TLS inventory; they are not added to the shared monitoring snapshot until its broader
wire contract is versioned.

Future metrics include active revision age, per-operation and directory-class totals, challenge
attempt duration, retry counters, activation/rollback outcomes, and durable audit records.

Metric labels MUST not include unbounded token, order URL, serial, or full domain-set data.
Audit events identify the certificate by configured stable name.

## Planned failure and recovery

- Restart resumes scheduler/account state but abandons in-flight issuance and creates a fresh order on the next attempt.
- Incomplete temporary revisions are ignored and cleaned after an age threshold.
- Account key loss is a hard diagnostic; no new account is silently substituted for the same configured identity.
- System clock problems block issuance/activation with a specific diagnostic.
- Rate-limit responses expose the retry time and do not spin.

## Hooks and extensions

Arbitrary pre/post/deploy shell hooks are excluded from the first release. Successful future
managed runtime publication will emit `certificate.activated`. A future hook runner requires
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
- DNS-01 provider success, propagation timeout, credential error, and cleanup failure.
- Nonce retry, order invalidation, polling timeout, rate limit, malformed chain, and key mismatch.
- Renewal-window and stable-jitter property tests with a fake clock.
- Crash/failure injection before and after every durable rename and runtime activation.
- Existing traffic continues through certificate rotation.
- Imported Certbot symlink changes and transient mixed revisions are handled without modifying its files.
- API and logs never contain private or account key bytes.
- State-root lock, owner/mode, no-follow, hard-link, unsafe-name, and non-local-filesystem failures.

Managed ACME tests use scripted transports and fake clocks. Production endpoints are not used by
CI; live validation remains the operator's staging-directory responsibility.
