# OxiRoute 0.5.2

OxiRoute 0.5.2 adds an explicit, opt-in retry path for selected requests whose bodies must be
replayed after an upstream response timeout. The release remains pre-alpha and retains
conservative compatibility claims.

## Highlights

- A bounded request-buffering policy can be explicitly enabled with `method_safety=all` and
  `body_safety=buffered` for declared idempotent requests with non-empty bodies. When enabled,
  OxiRoute can replay the buffered request after an upstream response timeout and continue the
  configured retry and redispatch policy.
- Retry safety remains opt-in: requests are not buffered or replayed unless the policy declares
  them eligible. The bounded buffer prevents retry support from turning request bodies into
  unbounded proxy memory use.
- Existing defaults remain conservative. The HAProxy importer does not infer that arbitrary `POST`
  requests are idempotent and does not enable buffered request replay from imported retry settings.

## Operational Notes

Use the retry path only for application operations that are genuinely idempotent. A replay can
reach an upstream that completed work but failed before delivering its response, so enabling it for
side-effecting operations can duplicate that work. Set the body-buffer limit to the largest request
that the selected safe operation needs, rather than using a broad global allowance.

## Verification Boundary

Coverage exercises eligible buffered replay after an upstream response timeout and preserves the
default behavior for non-empty requests that have not explicitly opted in. Release verification
also checks version alignment across workspace and Arch metadata. The final source archive checksum
is generated from the tagged release commit and then recorded in the Arch package metadata.
