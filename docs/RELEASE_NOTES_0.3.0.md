# OxiRoute 0.3.0

OxiRoute 0.3.0 adds the first tested pieces of cross-process zero-downtime supervision while
retaining the existing direct `serve` entry point as the public default.

## Highlights

- Add a socket-owning supervised master with debounced canonical configuration watching and
  periodic reconciliation.
- Replace same-listener-manifest workers through authenticated adoption, quiescence, activation,
  drain, rollback, and bounded reaping.
- Add worker-side active-generation quiesce, drain, and rollback reactivation support.
- Recover stale RTMP publisher ownership after 30 seconds without publisher media activity, while
  preserving recorder continuity and bounded retry behavior.
- Stage the hardened worker launcher and Arch package integration for the future supervised serve
  activation gate.

The supervised production path remains gated until the next release archive and launcher package
are published and the broader persistent-service migration is complete.
