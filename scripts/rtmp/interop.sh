#!/usr/bin/env bash
set -euo pipefail

# Run one bounded local RTMP publisher against an already-running OxiRoute listener.
# FFmpeg uses an in-memory lavfi source; no media or client output is written.

client=${RTMP_INTEROP_CLIENT:-ffmpeg}
url=${RTMP_INTEROP_URL:-rtmp://127.0.0.1:1935/live/interop}
duration=${RTMP_INTEROP_DURATION_SECONDS:-1}
timeout_seconds=${RTMP_INTEROP_TIMEOUT_SECONDS:-10}
ffmpeg_bin=${FFMPEG_BIN:-ffmpeg}
obs_bin=${OBS_BIN:-obs}
obs_profile=${OBS_PROFILE:-}

if ! [[ "$duration" =~ ^[1-9][0-9]*$ ]] || ((duration > 5)); then
  printf 'RTMP_INTEROP_DURATION_SECONDS must be an integer from 1 through 5\n' >&2
  exit 2
fi
if ! [[ "$timeout_seconds" =~ ^[1-9][0-9]*$ ]] || ((timeout_seconds > 30)); then
  printf 'RTMP_INTEROP_TIMEOUT_SECONDS must be an integer from 1 through 30\n' >&2
  exit 2
fi
if ! [[ "$url" =~ ^rtmp://(127\.0\.0\.1|localhost|\[::1\]):([0-9]{1,5})/([^/]+)/([^/]+)$ ]]; then
  printf 'RTMP_INTEROP_URL must target a loopback RTMP endpoint\n' >&2
  exit 2
fi
port=${BASH_REMATCH[2]}
if ((10#$port < 1 || 10#$port > 65535)); then
  printf 'RTMP_INTEROP_URL port must be between 1 and 65535\n' >&2
  exit 2
fi
if [[ "$url" == *'@'* || "$url" == *'?'* || "$url" == *'#'* ]]; then
  printf 'RTMP_INTEROP_URL must not contain credentials, queries, or fragments\n' >&2
  exit 2
fi
if ! command -v timeout >/dev/null 2>&1; then
  printf 'timeout is required for bounded RTMP interoperability checks\n' >&2
  exit 2
fi

case "$client" in
  ffmpeg)
    command -v "$ffmpeg_bin" >/dev/null 2>&1 || {
      printf 'FFmpeg executable is unavailable\n' >&2
      exit 2
    }
    if timeout --signal=TERM --kill-after=2s "$timeout_seconds" \
      "$ffmpeg_bin" \
      -hide_banner \
      -loglevel error \
      -f lavfi \
      -i 'color=c=black:size=16x16:rate=1' \
      -t "$duration" \
      -f flv \
      "$url" >/dev/null 2>&1; then
      printf 'FFmpeg RTMP publish passed\n'
    else
      status=$?
      printf 'FFmpeg RTMP publish failed with exit %d\n' "$status" >&2
      exit 1
    fi
    ;;
  obs)
    command -v "$obs_bin" >/dev/null 2>&1 || {
      printf 'OBS executable is unavailable\n' >&2
      exit 2
    }
    if [[ -z "$obs_profile" ]]; then
      printf 'OBS must use a preconfigured loopback RTMP profile; set OBS_PROFILE if needed\n' >&2
      exit 2
    fi
    if timeout --signal=TERM --kill-after=2s "$timeout_seconds" \
      "$obs_bin" \
      --profile "$obs_profile" \
      --startstreaming \
      --minimize-to-tray >/dev/null 2>&1; then
      printf 'OBS RTMP publish passed\n'
    else
      status=$?
      printf 'OBS RTMP publish failed with exit %d\n' "$status" >&2
      exit 1
    fi
    ;;
  *)
    printf 'RTMP_INTEROP_CLIENT must be ffmpeg or obs\n' >&2
    exit 2
    ;;
esac
