#!/usr/bin/env python3
"""Live bytes of a jemalloc heap profile dump, grouped by allocation site (the first DEPTH frames of the stack that are
not allocator plumbing). Reads the dump itself and symbolizes with addr2line, so the binary needs line tables.

  1. Build a binary whose allocator has profiling: `tikv-jemallocator = { workspace = true, features = ["profiling"] }`
     in `crates/oxiroute-allocator/Cargo.toml` (a local edit, do not commit it).
  2. Run it with `_RJEM_MALLOC_CONF=prof:true,prof_active:true,lg_prof_sample:9,lg_prof_interval:22,prof_prefix:DIR/j`
     (a dump every 4 MiB allocated) while the connections of the scenario are open.
  3. python3 heapsum.py BIN DIR/j.<pid>.<last>.i<last>.heap [depth] [top]

Sampled sizes are estimates; the profile itself roughly doubles the resident memory, so take the resident figures of a
build without it. Objects of a block that is never touched (a buffer that was allocated but not written) count here and
not in the resident memory.
"""
import collections
import re
import subprocess
import sys
binp, dump = sys.argv[1], sys.argv[2]
depth = int(sys.argv[3]) if len(sys.argv) > 3 else 6
top = int(sys.argv[4]) if len(sys.argv) > 4 else 40
text = open(dump).read()
maps = re.findall(r'^([0-9a-f]+)-[0-9a-f]+ \S+ ([0-9a-f]+) .*' + re.escape(binp.split('/')[-1]) + '$', text, re.M)
base = int(maps[0][0], 16)
recs = re.findall(r'^@ ([0-9a-fx ]+)\n\s+t\*: (\d+): (\d+) ', text, re.M)
addrs = set()
stacks = []
for st, n, b in recs:
    fr = [int(a, 16) - base for a in st.split()]
    stacks.append((fr, int(n), int(b)))
    addrs.update(fr)
addrs = sorted(addrs)
out = subprocess.run(['addr2line', '-e', binp, '-f', '-C', '-i', '-p'] + [hex(a) for a in addrs], capture_output=True, text=True, env={'LC_ALL': 'C'}).stdout
# addr2line -p -i prints "func at file:line" per line, inlined frames on following lines starting with " (inlined by)"
res = {}; cur = None
for line in out.splitlines():
    if line.startswith(' (inlined by)'):
        res[cur].append(line.replace(' (inlined by) ', ''))
    else:
        cur = addrs[len(res)]; res[cur] = [line]
def clean(s):
    s = re.sub(r' at /(rustc|home)/\S*?/(src|crates|vendor)/', ' @ ', s)
    return s[:110]
skip = re.compile(r'alloc::|jemalloc|prof_|malloc|_rjem|<alloc|core::|RawVec|Vec<|Box<|raw_vec|__rust|alloc_impl|finish_grow|::allocate|realloc|with_capacity|from_iter|clone|spec_|extend|to_vec|reserve', re.I)
agg = collections.Counter(); cnt = collections.Counter()
for fr, n, b in stacks:
    frames = []
    for a in fr:
        frames += res.get(a, ['?'])
    keep = [clean(f) for f in frames if not skip.search(f.split(' at ')[0]) and 'jemalloc' not in f and 'post_reentrancy' not in f]
    key = ' < '.join(keep[:depth])
    agg[key] += b; cnt[key] += n
tot = sum(agg.values())
print(f'total {tot/1e6:.1f} MB, objects {sum(cnt.values())}')
for k, v in agg.most_common(top):
    print(f'{v/1e6:8.1f} MB {cnt[k]:8d} obj  {k}')
