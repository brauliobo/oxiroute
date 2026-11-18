#!/usr/bin/env bash
# Prints the identity of this machine's CPU: a digest of the model name and the feature flags of the first core
# (CPUINFO overrides /proc/cpuinfo). Objects built with `-C target-cpu=native` are valid only on the CPU that printed
# the same identity, because cargo fingerprints `native` by name and not by the instructions it selected.
set -euo pipefail

grep -E -m2 '^(model name|flags)' "${CPUINFO:-/proc/cpuinfo}" | sha256sum | cut -c1-16
