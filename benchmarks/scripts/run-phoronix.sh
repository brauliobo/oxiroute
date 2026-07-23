#!/usr/bin/env bash

set -euo pipefail
IFS=$'\n\t'

source "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"

command -v phoronix-test-suite >/dev/null 2>&1 || die "phoronix-test-suite is unavailable"

pts_root="$GENERATED_ROOT/phoronix"
profile_root="$pts_root/user/test-profiles/local"
profile_link="$profile_root/oxiroute-local-v1"
mkdir -p -- "$profile_root"
if [[ -L $profile_link ]]; then
  rm -- "$profile_link"
elif [[ -e $profile_link ]]; then
  die "refusing to replace non-symlink profile path: $profile_link"
fi
ln -s -- "$BENCHMARK_ROOT/phoronix/local/oxiroute-local-v1" "$profile_link"

export OXIROUTE_BENCHMARK_ROOT=$BENCHMARK_ROOT
export PTS_USER_PATH_OVERRIDE="$pts_root/user/"
export PTS_SILENT_MODE=1
export PTS_BATCH_MODE=1

phoronix-test-suite user-config-set \
  PhoronixTestSuite/Options/Installation/CacheDirectory="$pts_root/download-cache/" \
  PhoronixTestSuite/Options/Installation/EnvironmentDirectory="$pts_root/installed-tests/" \
  PhoronixTestSuite/Options/Modules/AutoLoadModules= \
  PhoronixTestSuite/Options/OpenBenchmarking/AllowResultUploadsToOpenBenchmarking=FALSE \
  PhoronixTestSuite/Options/OpenBenchmarking/AnonymousUsageReporting=FALSE \
  PhoronixTestSuite/Options/BatchMode/Configured=TRUE \
  PhoronixTestSuite/Options/BatchMode/OpenBrowser=FALSE \
  PhoronixTestSuite/Options/BatchMode/PromptForTestDescription=FALSE \
  PhoronixTestSuite/Options/BatchMode/PromptForTestIdentifier=FALSE \
  PhoronixTestSuite/Options/BatchMode/PromptSaveName=FALSE \
  PhoronixTestSuite/Options/BatchMode/RunAllTestCombinations=TRUE \
  PhoronixTestSuite/Options/BatchMode/SaveResults=TRUE \
  PhoronixTestSuite/Options/BatchMode/UploadResults=FALSE \
  PhoronixTestSuite/Options/Networking/NoInternetCommunication=TRUE \
  PhoronixTestSuite/Options/Networking/NoNetworkCommunication=TRUE \
  PhoronixTestSuite/Options/Server/PhoromaticStorage="$pts_root/phoromatic/" \
  PhoronixTestSuite/Options/Testing/AlwaysUploadResultsToOpenBenchmarking=FALSE \
  PhoronixTestSuite/Options/Testing/ResultsDirectory="$pts_root/results/" \
  PhoronixTestSuite/Options/TestResultValidation/DynamicRunCount=FALSE
phoronix-test-suite install local/oxiroute-local-v1
phoronix-test-suite batch-run local/oxiroute-local-v1
