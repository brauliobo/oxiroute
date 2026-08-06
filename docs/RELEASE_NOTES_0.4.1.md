# OxiRoute 0.4.1

OxiRoute 0.4.1 adds opt-in RFC 9298 `CONNECT-UDP` support to the HTTP/1.1 forward proxy while
keeping existing CONNECT behavior and defaults unchanged.

## Highlights

- Add bounded Capsule Protocol DATAGRAM relay to connected UDP destinations.
- Add separate typed configuration and policy controls for allowed CONNECT-UDP ports.
- Expose the feature through validation, topology, native-import defaults, and the Vue dashboard.
