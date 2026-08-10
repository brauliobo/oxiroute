#!/usr/bin/env bash

RELEASE_ARCHIVE_EXCLUDES=(
  ':(exclude)packaging/arch/**'
  ':(exclude)benchmarks/reports/**'
  ':(exclude)target/**'
  ':(exclude)**/target/**'
  ':(exclude)node_modules/**'
  ':(exclude)**/node_modules/**'
  ':(exclude)remotion/out/**'
  ':(exclude)test-results/**'
)

RELEASE_REQUIRED_PATHS=(
  Cargo.lock
  Cargo.toml
  LICENSE
  fuzz/Cargo.lock
  benchmarks/loadgen/Cargo.lock
  ui/pnpm-lock.yaml
  remotion/pnpm-lock.yaml
)

RELEASE_ALLOWED_SECRET_PATHS=(
  crates/oxiroute-import/tests/fixtures/haproxy/tls-chain.pem.key
  crates/oxiroute-import/tests/fixtures/haproxy/tls-no-identities.pem.key
  crates/oxiroute-import/tests/fixtures/nginx/proxy-key.pem
  crates/oxiroute-import/tests/fixtures/nginx/proxy-mismatched-key.pem
  crates/oxiroute-server/src/tls/tests.rs
  crates/oxiroute-server/tests/fixtures/origin-key.pem
  crates/oxiroute-server/tests/fixtures/proxy-a-key.pem
  crates/oxiroute-server/tests/fixtures/proxy-b-key.pem
  vendor/pingora-core/examples/keys/client-ca/key.pem
  vendor/pingora-core/examples/keys/clients/invalid-key.pem
  vendor/pingora-core/examples/keys/clients/key-1.pem
  vendor/pingora-core/examples/keys/clients/key-2.pem
  vendor/pingora-core/examples/keys/server/key.pem
)

RELEASE_DENIED_PATH_PATTERNS=(
  target 'target/*' '*/target' '*/target/*'
  node_modules 'node_modules/*' '*/node_modules' '*/node_modules/*'
  remotion/out 'remotion/out/*' '*/remotion/out' '*/remotion/out/*'
  test-results 'test-results/*' '*/test-results' '*/test-results/*'
  benchmarks/reports 'benchmarks/reports/*' '*/benchmarks/reports' '*/benchmarks/reports/*'
  .git '.git/*' '*/.git' '*/.git/*'
)

RELEASE_SECRET_PATH_PATTERNS=(
  '*.env' '*.env.*' '*.token' '*.secret' '*.secrets' '*credentials*'
  '*.key' '*.p12' '*.pfx' '*/id_rsa' '*/id_ed25519'
)

RELEASE_SECRET_CONTENT_PATTERN='-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----|(^|[^A-Za-z0-9])(AKIA|ASIA)[0-9A-Z]{16}([^A-Za-z0-9]|$)|(^|[^A-Za-z0-9])gh[pousr]_[A-Za-z0-9_]{20,}([^A-Za-z0-9]|$)|(^|[^A-Za-z0-9])github_pat_[A-Za-z0-9_]{20,}([^A-Za-z0-9]|$)|(^|[^A-Za-z0-9])xox[baprs]-[A-Za-z0-9-]{20,}([^A-Za-z0-9]|$)|(^|[^A-Za-z0-9])npm_[A-Za-z0-9]{20,}([^A-Za-z0-9]|$)|(^|[^A-Za-z0-9])sk_live_[A-Za-z0-9]{16,}([^A-Za-z0-9]|$)'
export RELEASE_SECRET_CONTENT_PATTERN
