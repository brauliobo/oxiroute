# OxiRoute 0.5.3

OxiRoute 0.5.3 makes a configuration reload apply listener changes. Adding, removing or moving a
listener no longer needs a service restart, and the traffic on every other listener keeps flowing.
The release remains pre-alpha and retains conservative compatibility claims.

## Highlights

- A reload that adds a listener binds the new socket before any traffic switches. If the bind fails,
  the current revision stays active, nothing changes, and the reason is logged (the management API
  already rejects nearly every such candidate when it is validated, because validation binds the
  addresses the running generation does not own).
- A reload that removes a listener makes it stop accepting at the moment the new revision is
  committed. Its address can be bound again immediately. Connections it had already accepted keep
  being served by the replaced worker until they finish or the drain window ends, so long-lived
  streams and the multi-hour read timeouts of analysis and transcription services are not cut.
- Moving a listener to another address, or changing its protocol, service or TLS profile, is an add
  plus a remove, paired by bind identity (kind, address and mode) rather than by name or position.
  A listener that keeps its address and gets a new TLS profile is served by the new worker with the
  new profile; connections already open on it finish on the replaced worker with the old one.
- Unchanged listeners keep their sockets and are never rebound, so their accept queues and
  in-flight requests are untouched.
- The management API, the CLI and the UI list what a candidate changes: an `I_LISTENER_CHANGES`
  diagnostic (severity `warning`, stage `activation`, path `/config/listeners`) names the listeners
  added, removed and kept, and the supervised master logs the same summary when it starts a revision.
  Revision, rollback, metrics and audit behaviour are unchanged.
- Both runtimes follow the same rule. The old 0.5.2 message that the supervised master retained a
  reload that changes listeners is gone.

## Operational Notes

Upgrade note: the running process must be replaced once to get this behaviour. A 0.5.2 process that
is reloaded still retains a listener-changing configuration, so install 0.5.3 and restart the service
one time. From then on, listener changes are applied by `systemctl reload oxiroute.service`, by the
configuration API and by the file watcher.

Drain window: the replaced worker keeps serving accepted connections for at most
`OXIROUTE_RELOAD_DRAIN_SECONDS` (default 10800, three hours; accepted range 1 to 86400). An invalid
value is a startup error that names the variable. The packaged `oxiroute.env` documents it as a
commented assignment.

Limits to know about:

- Only one replaced worker drains at a time. While one is still serving a long-lived connection, a
  further reload that needs a new worker waits; the periodic reconciliation applies the newest
  revision as soon as the drain ends, or the next restart does.
- An idle HTTP keep-alive connection is closed when its worker drains. Clients reconnect to the new
  worker; requests in flight and streaming responses are not interrupted.
- A bind failure found only by the supervised master (after the API validated the candidate) is
  reported in the service log, once per revision, and retried by the periodic reconciliation.
- A Unix listener that keeps its path and changes only its filesystem mode still needs a restart,
  because the new socket would need the path the running one owns. It is saved as restart-required
  and everything else in that candidate waits with it.

Rollback: install the previous package and restart the service. The configuration format is
unchanged, so no configuration edit is needed.

## Verification Boundary

Coverage includes unit tests of the descriptor diff and listener labels, master tests of the swap at
commit, the shutdown of removed listeners and rollback, supervised integration tests with the real
worker for add, remove, move, bind-failure retention and a stream that survives a reload, API tests
of the listener-changes diagnostic in both runtimes, and a UI test of the review banner. The
`scripts/verify-public-api.sh` gate pins Rust 1.97.1 and was not run on a host with a different
toolchain. Release verification also checks version alignment across workspace, UI and Arch
metadata. The final source archive checksum is generated from the tagged release commit and then
recorded in the Arch package metadata.
