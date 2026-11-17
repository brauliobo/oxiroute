#!/usr/bin/env bash
# The stage timer must not change what the function it runs sees.
set -euo pipefail

source "$(dirname -- "${BASH_SOURCE[0]}")/../lib/stage/time.sh"

name=kept label=kept began=kept status=kept line=kept
seen() { [[ ${name} == kept && ${label} == kept && ${began} == kept && ${status} == kept && ${line} == kept ]]; }
timed pack seen 2> /dev/null
timed pack false 2> /dev/null && { echo 'the timer hid a failure' >&2; exit 1; }
echo 'verified the stage timer keeps the variables of the function it runs'
