#!/usr/bin/env bash

# The bounded smoke and corpus checks intentionally share one target contract.
FUZZ_TARGET_SPECS=(
    "config_source:131072"
    "native_source:131072"
    "forward_target:16384"
    "overread_io:16384"
    "rtmp_handshake:131072"
    "rtmp_chunk:262144"
    "rtmp_amf:32768"
    "rtmp_media_config:65536"
    "proxy_protocol:131072"
    "udp_datagram:131059"
    "tls_client_hello:65536"
    "http1:131072"
)
