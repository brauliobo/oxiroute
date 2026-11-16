"""Cache-hit lanes: each proves that its measured window reached the origin never, so every response was a hit."""
from pathlib import Path


def cache_hit_lanes(lane):
    def hit(name, path, size, conns, **more):
        return lane(name, ["--path", f"/{path}", "--expect-len", str(size), "--conns", str(conns)], features=["cache"], **more)

    return [hit("cache_hit_b1k", "b1k", 1024, 16), hit("cache_hit_b64k", "b64k", 65536, 16, requests=5000),
            hit("cache_hit_b1k_w2", "b1k", 1024, 64, requests=40000, warmup=2000, workers=2),
            hit("cache_hit_b64k_w2", "b64k", 65536, 64, requests=10000, warmup=2000, workers=2)]


def cpu_pressure():
    """The `some` avg10 of /proc/pressure/cpu, in percent (diagnostic only)."""
    return Path("/proc/pressure/cpu").read_text().splitlines()[0].split()[1].split("=")[1]


def served_probe(lane, work, rep, tag, origin_port):
    """Shell commands that store how many requests the origin answered so far (`/__served` of `r2tool upstream`)."""
    if not lane.name.startswith("cache_hit"):
        return []
    return [f"curl -s http://127.0.0.1:{origin_port}/__served > {work}/served{rep}.{tag}"]


def require_only_hits(lane, work, rep):
    if lane.name.startswith("cache_hit"):
        first, last = (int((work / f"served{rep}.{tag}").read_text()) for tag in ("a", "b"))
        if last != first:
            raise RuntimeError(f"{lane.name}: {last - first} measured requests reached the origin, not every one was a hit")
