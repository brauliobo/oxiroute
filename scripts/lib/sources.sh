#!/usr/bin/env bash
# Sourced by the scripts/verify checks: the one list of source files they police.

source_extensions='rs|ts|vue|js|mjs|py|sh'
source_excluded='^(vendor|fuzz|website|remotion|target)/|^ui/src/(api|stats)/generated/'
source_excluded+='|^crates/oxiroute-server/src/observability/stats_page/assets/table\.rs$'

tracked_sources() {
  git -C "$1" ls-files -z | grep -zE "\.(${source_extensions})\$" | grep -zvE "${source_excluded}"
}
