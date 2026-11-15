# Vendored pingora-timeout patch

This directory contains the published `pingora-timeout 0.9.1` source from crates.io
(registry checksum `d58d029af71f7694815494c681f1bb93aa06d83b6fa6a6b42e7154ee0e8e43e4`)
under its upstream Apache-2.0 license. The benchmark target and its dev-dependency are dropped.

OxiRoute's delta is limited to the clock thread in `src/timer.rs`:

- Upstream wakes the `Timer thread` every 10 ms for the life of the process (about 100 wakeups per
  second on an idle instance). Here it fires the timers that are due and then parks until the earliest
  armed timer, or until a timer is armed while nothing is.
- `register_timer` unparks the thread only when the new timer is due before the time it sleeps until,
  so the request path costs one atomic load.
- The clock watchdog is gone: a flag says whether the clock thread was started, and `unpause()` clears it in a
  forked process, which does not have the thread, so an idle instance never wakes the thread.
- Timers are armed one millisecond later than upstream computed, because the thread now wakes at the exact
  due time and a timer must not fire early.
- A pending timeout reads the clock once, for its timer bucket: the start check of the clock thread is one flag.
- A pending timeout boxes its timer future once: `timeout()` already returns a boxed future, which was boxed again.
- `fast_timeout.rs` keeps upstream's 0.9 rule that a long timeout uses Tokio's timer (`FastOrTokioTimeout`); the clock
  thread now starts when a fast timer is first registered, so a future that completes at once reads no clock.
