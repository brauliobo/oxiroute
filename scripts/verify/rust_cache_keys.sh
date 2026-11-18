#!/usr/bin/env bash
# Fails when a workflow caches build output under a key that cannot tell the CPUs of the runners apart, or under a key
# input that Swatinem/rust-cache ignores.
#
#   rust_cache_keys.sh [REPO_DIR]
#
# Swatinem/rust-cache builds its key from `shared-key`, or else from `key` and the job id: with both set, `key` is
# dropped without a message. A job that builds with `-C target-cpu=native` (it runs scripts/test/cpu-targets.sh) must
# set `key` to the output of a step that runs scripts/lib/cpu_id.sh, so that a runner never restores objects built
# for another CPU model.
set -euo pipefail

root=${1:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)}

python3 -I - "${root}/.github/workflows" <<'PY'
import sys
from pathlib import Path

import yaml

NATIVE_LANE = 'cpu-targets.sh'
CPU_ID = 'cpu_id.sh'
GUIDANCE = (
    'Fix the cache key itself: give a rust-cache step either shared-key or key, and key the cache of a native-CPU lane '
    'by the output of a step that runs scripts/lib/cpu_id.sh. Do not tolerate stale objects or retry the lane.'
)

errors = []
for workflow in sorted(Path(sys.argv[1]).glob('*.yml')):
    for job_id, job in (yaml.safe_load(workflow.read_text()).get('jobs') or {}).items():
        steps = job.get('steps') or []
        cpu_steps = {step.get('id') for step in steps if CPU_ID in (step.get('run') or '')}
        native = any(NATIVE_LANE in (step.get('run') or '') for step in steps)
        for step in steps:
            if not str(step.get('uses') or '').startswith('Swatinem/rust-cache'):
                continue
            inputs = step.get('with') or {}
            where = f'{workflow.name}: job {job_id}'
            if 'shared-key' in inputs and 'key' in inputs:
                errors.append(f'{where}: key is ignored when shared-key is set, so the cache is shared by every runner')
            if native and not any(f'steps.{cpu}.outputs.' in str(inputs.get('key') or '') for cpu in cpu_steps):
                errors.append(f'{where}: a native-CPU lane caches under a key that does not carry the runner CPU')
if errors:
    print('\n'.join(errors) + '\n' + GUIDANCE, file=sys.stderr)
    sys.exit(1)
print('verified every rust-cache key')
PY
