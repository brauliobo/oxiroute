#!/usr/bin/env python3

import argparse
import hashlib
import json
import os
import pathlib
import platform
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

BENCHMARK_ROOT = pathlib.Path(__file__).resolve().parent.parent
REPORTS_ROOT = BENCHMARK_ROOT / "reports"
GENERATED_RUNS_ROOT = BENCHMARK_ROOT / "generated" / "runs"
RUST_TOOLCHAIN = "1.87.0"
EMPTY_SHA256 = hashlib.sha256(b"").hexdigest()
HISTORICAL_REPORTS = {
    "2026-07-23-local-v1.json",
    "2026-07-23-local-v2.json",
    "2026-07-23-optimization-v1.json",
}


def write_json(path, value):
    destination = pathlib.Path(path)
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def write_json_atomic(path, value):
    destination = pathlib.Path(path)
    descriptor, temporary_name = tempfile.mkstemp(prefix=f".{destination.name}.", dir=destination.parent)
    os.close(descriptor)
    temporary = pathlib.Path(temporary_name)
    try:
        temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        temporary.chmod(destination.stat().st_mode & 0o777)
        os.replace(temporary, destination)
    finally:
        if temporary.exists():
            temporary.unlink()


def command_version(command, arguments):
    path = shutil.which(command)
    if path is None:
        return {
            "available": False,
            "path": None,
            "resolved_path": None,
            "sha256": None,
            "version": None,
            "version_arguments": arguments,
            "version_exit_status": None,
        }
    executable = pathlib.Path(path).absolute()
    resolved = executable.resolve()
    try:
        result = subprocess.run(
            [executable, *arguments], capture_output=True, check=False, text=True, timeout=10
        )
        version = (result.stdout + result.stderr).strip() or None
        exit_status = result.returncode
    except (OSError, subprocess.SubprocessError) as error:
        version = f"version query failed: {error.__class__.__name__}"
        exit_status = None
    return {
        "available": True,
        "path": str(executable),
        "resolved_path": str(resolved),
        "sha256": sha256_file(resolved),
        "version": version,
        "version_arguments": arguments,
        "version_exit_status": exit_status,
    }


def executable_metadata(path):
    executable = pathlib.Path(path).absolute()
    resolved = executable.resolve()
    available = executable.is_file() and os.access(executable, os.X_OK)
    return {
        "available": available,
        "path": str(executable),
        "resolved_path": str(resolved),
        "sha256": sha256_file(resolved) if available else None,
    }


def is_below(path, parent):
    try:
        path.relative_to(parent)
        return True
    except ValueError:
        return False


def render(args):
    if len(args.replacement) % 2:
        raise SystemExit("render replacements must be TOKEN VALUE pairs")
    source = pathlib.Path(args.input).read_text(encoding="utf-8")
    for token, value in zip(args.replacement[::2], args.replacement[1::2]):
        source = source.replace(f"@{token}@", value)
    unresolved = sorted(set(re.findall(r"@[A-Z][A-Z0-9_]*@", source)))
    if unresolved:
        raise SystemExit(f"unresolved template tokens: {', '.join(unresolved)}")
    destination = pathlib.Path(args.output)
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(source, encoding="utf-8")


def wait_http(args):
    deadline = time.monotonic() + args.timeout
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(args.url, timeout=0.5) as response:
                body = response.read()
                if response.status == 200 and (
                    args.expected_bytes is None or len(body) == args.expected_bytes
                ):
                    return
        except (OSError, urllib.error.URLError):
            time.sleep(0.05)
    raise SystemExit(1)


def port_available(port):
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.bind(("127.0.0.1", port))
        return True
    except OSError:
        return False
    finally:
        sock.close()


def preflight(args):
    tools = {
        "bash": command_version("bash", ["--version"]),
        "haproxy": command_version("haproxy", ["-v"]),
        "loadgen": command_version(args.loadgen_bin, ["--version"]),
        "nginx": command_version("nginx", ["-v"]),
        "python3": command_version("python3", ["--version"]),
        "taskset": command_version("taskset", ["--version"]),
    }
    tools["oxiroute"] = executable_metadata(args.oxiroute_bin)
    tools["oxiroute"]["version"] = None
    required = {"bash", "loadgen", "nginx", "python3", "taskset"}
    required.update(implementation for implementation in args.implementations if implementation != "origin")
    ok = all(tools[name]["available"] for name in required)
    selected_cpus = [args.proxy_cpu, args.origin_cpu, args.load_cpu]
    online_cpus = sorted(os.sched_getaffinity(0))
    cpu_affinity_ok = len(set(selected_cpus)) == 3 and all(
        cpu in online_cpus for cpu in selected_cpus
    )
    ok = ok and cpu_affinity_ok
    ports = {
        str(args.origin_port): port_available(args.origin_port),
        str(args.proxy_port): port_available(args.proxy_port),
    }
    ok = ok and all(ports.values()) and sys.platform.startswith("linux") and pathlib.Path("/proc").is_dir()
    report = {
        "schema": "oxiroute.local-v1.preflight.v1",
        "ok": ok,
        "implementations": args.implementations,
        "linux": sys.platform.startswith("linux"),
        "proc_available": pathlib.Path("/proc").is_dir(),
        "ports_available": ports,
        "cpu_affinity": {
            "ok": cpu_affinity_ok,
            "online": online_cpus,
            "selected": {
                "proxy": args.proxy_cpu,
                "origin": args.origin_cpu,
                "load_generator": args.load_cpu,
            },
        },
        "tools": tools,
    }
    write_json(args.output, report)
    if not ok:
        raise SystemExit(1)


def read_text_command(arguments):
    try:
        result = subprocess.run(arguments, capture_output=True, check=False, text=True, timeout=10)
    except (OSError, subprocess.SubprocessError):
        return None
    return result.stdout.strip() or result.stderr.strip() or None


def rust_tool_version(command):
    rustup = shutil.which("rustup")
    if rustup is None:
        return command_version(f"missing-{command}", ["-Vv"])
    try:
        result = subprocess.run(
            [rustup, "which", "--toolchain", RUST_TOOLCHAIN, command],
            capture_output=True,
            check=False,
            text=True,
            timeout=10,
        )
    except (OSError, subprocess.SubprocessError):
        return command_version(f"missing-{command}", ["-Vv"])
    path = result.stdout.strip() if result.returncode == 0 else f"missing-{command}"
    return command_version(path, ["-Vv"])


def sha256_file(path):
    digest = hashlib.sha256()
    with pathlib.Path(path).open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def command_bytes(arguments):
    try:
        result = subprocess.run(arguments, capture_output=True, check=False, timeout=10)
    except (OSError, subprocess.SubprocessError):
        return None
    return result.stdout if result.returncode == 0 else None


def environment(args):
    repository = pathlib.Path(args.repository).resolve()
    cpu = {}
    cpuinfo = pathlib.Path("/proc/cpuinfo")
    if cpuinfo.is_file():
        for line in cpuinfo.read_text(encoding="utf-8", errors="replace").splitlines():
            if ":" not in line:
                continue
            key, value = (item.strip() for item in line.split(":", 1))
            if key in {"model name", "Hardware"} and "model" not in cpu:
                cpu["model"] = value
    cpu["logical_count"] = os.cpu_count()
    git_head = read_text_command(["git", "-C", str(repository), "rev-parse", "HEAD"])
    git_status_output = command_bytes(
        ["git", "-C", str(repository), "status", "--porcelain=v1", "--untracked-files=all"]
    )
    git_status = os.fsdecode(git_status_output).strip() if git_status_output is not None else None
    git_diff = command_bytes(["git", "-C", str(repository), "diff", "--binary", "HEAD", "--"])
    untracked_output = command_bytes(
        ["git", "-C", str(repository), "ls-files", "--others", "--exclude-standard", "-z"]
    )
    untracked = []
    if untracked_output is not None:
        relative_paths = sorted(
            os.fsdecode(path) for path in untracked_output.split(b"\0") if path
        )
        for relative in relative_paths:
            path = repository / relative
            if path.is_symlink():
                target = os.readlink(path)
                untracked.append(
                    {
                        "path": relative,
                        "kind": "symlink",
                        "target": target,
                        "sha256": hashlib.sha256(os.fsencode(target)).hexdigest(),
                    }
                )
            else:
                untracked.append(
                    {"path": relative, "kind": "file", "sha256": sha256_file(path)}
                )
    oxiroute = executable_metadata(args.oxiroute_bin)
    loadgen = executable_metadata(args.loadgen_bin)
    tools = {
        "cargo": rust_tool_version("cargo"),
        "haproxy": command_version("haproxy", ["-v"]),
        "loadgen": command_version(args.loadgen_bin, ["--version"]),
        "nginx": command_version("nginx", ["-v"]),
        "phoronix-test-suite": command_version("phoronix-test-suite", ["version"]),
        "python3": command_version("python3", ["--version"]),
        "rustc": rust_tool_version("rustc"),
        "taskset": command_version("taskset", ["--version"]),
    }
    report = {
        "schema": "oxiroute.local-v1.environment.v2",
        "captured_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "cpu": cpu,
        "git": {
            "head": git_head,
            "dirty": bool(git_status),
            "status_porcelain_v1": git_status or "",
            "tracked_diff_sha256": hashlib.sha256(git_diff).hexdigest()
            if git_diff is not None
            else None,
            "untracked_files": untracked,
        },
        "repository_path": str(repository),
        "rust_toolchain": RUST_TOOLCHAIN,
        "oxiroute_binary": oxiroute,
        "loadgen_binary": loadgen,
        "platform": {
            "machine": platform.machine(),
            "release": platform.release(),
            "system": platform.system(),
        },
        "tools": tools,
    }
    write_json(args.output, report)
    if git_head is None or git_status is None or git_diff is None or untracked_output is None:
        raise SystemExit("source worktree provenance could not be captured")
    if any(tools[name]["version_exit_status"] != 0 for name in ("cargo", "rustc")):
        raise SystemExit(f"pinned Rust toolchain {RUST_TOOLCHAIN} is unavailable")


def run_metadata(args):
    write_json(
        args.output,
        {
            "schema": "oxiroute.local-v1.run.v2",
            "implementation": args.implementation,
            "origin_port": args.origin_port,
            "proxy_port": args.proxy_port,
            "load_generator": "oxiroute-loadgen",
            "connections": args.connections,
            "warmup_seconds": args.warmup_seconds,
            "duration_seconds": args.duration_seconds,
            "cpu_affinity": {
                "proxy": args.proxy_cpu,
                "origin": args.origin_cpu,
                "load_generator": args.load_cpu,
            },
            "provenance_file": "environment.json",
        },
    )


def report_run_ids(value):
    run_ids = set()
    if isinstance(value, dict):
        for key, child in value.items():
            if key == "evidence":
                continue
            if key == "run_ids":
                if not isinstance(child, list) or not child:
                    raise SystemExit("report run_ids must be a nonempty array")
                if not all(
                    isinstance(item, str)
                    and item
                    and pathlib.PurePath(item).name == item
                    and re.fullmatch(r"[A-Za-z0-9._-]+", item)
                    for item in child
                ):
                    raise SystemExit("report run_ids contain an invalid directory name")
                run_ids.update(child)
            else:
                run_ids.update(report_run_ids(child))
    elif isinstance(value, list):
        for child in value:
            run_ids.update(report_run_ids(child))
    return run_ids


def canonical_report_bytes(report):
    content = {key: value for key, value in report.items() if key != "evidence"}
    return json.dumps(content, ensure_ascii=False, separators=(",", ":"), sort_keys=True).encode()


def require_publishable_report_path(supplied, resolved):
    if (
        supplied.parent != REPORTS_ROOT
        or resolved.parent != REPORTS_ROOT
        or supplied.suffix != ".json"
        or resolved.suffix != ".json"
    ):
        raise SystemExit(f"report must be a JSON file directly below {REPORTS_ROOT}")


def require_fields(value, fields, label):
    if not isinstance(value, dict) or not fields.issubset(value):
        raise SystemExit(f"{label} is missing required provenance fields")


def require_executable(value, label):
    require_fields(value, {"path", "resolved_path", "sha256"}, label)
    if not all(isinstance(value[key], str) and value[key] for key in ("path", "resolved_path")):
        raise SystemExit(f"{label} executable paths are missing")
    if not isinstance(value["sha256"], str) or re.fullmatch(r"[0-9a-f]{64}", value["sha256"]) is None:
        raise SystemExit(f"{label} executable hash is missing")


def validate_run_provenance(run):
    run_metadata = json.loads((run / "run.json").read_text(encoding="utf-8"))
    environment = json.loads((run / "environment.json").read_text(encoding="utf-8"))
    if run_metadata.get("schema") != "oxiroute.local-v1.run.v2":
        raise SystemExit(f"run does not use the provenance contract: {run}")
    if run_metadata.get("provenance_file") != "environment.json":
        raise SystemExit(f"run does not reference environment.json: {run}")
    if environment.get("schema") != "oxiroute.local-v1.environment.v2":
        raise SystemExit(f"environment does not use the provenance contract: {run}")
    if environment.get("rust_toolchain") != RUST_TOOLCHAIN:
        raise SystemExit(f"environment does not use Rust {RUST_TOOLCHAIN}: {run}")

    git = environment.get("git")
    require_fields(
        git,
        {"head", "dirty", "status_porcelain_v1", "tracked_diff_sha256", "untracked_files"},
        "git metadata",
    )
    if not isinstance(git["head"], str) or re.fullmatch(r"[0-9a-f]{40,64}", git["head"]) is None:
        raise SystemExit(f"source commit is missing: {run}")
    if (
        not isinstance(git["dirty"], bool)
        or git["dirty"] != bool(git["status_porcelain_v1"])
        or not isinstance(git["status_porcelain_v1"], str)
        or not isinstance(git["untracked_files"], list)
    ):
        raise SystemExit(f"source worktree state is missing: {run}")
    if not isinstance(git["tracked_diff_sha256"], str) or re.fullmatch(
        r"[0-9a-f]{64}", git["tracked_diff_sha256"]
    ) is None:
        raise SystemExit(f"source worktree fingerprint is missing: {run}")
    for item in git["untracked_files"]:
        require_fields(item, {"path", "kind", "sha256"}, "untracked file metadata")
        if (
            not isinstance(item["path"], str)
            or item["kind"] not in {"file", "symlink"}
            or not isinstance(item["sha256"], str)
            or re.fullmatch(r"[0-9a-f]{64}", item["sha256"]) is None
        ):
            raise SystemExit(f"untracked file fingerprint is missing: {run}")
    if git["dirty"] or git["status_porcelain_v1"] or git["untracked_files"]:
        raise SystemExit(f"publishable evidence requires a clean source worktree: {run}")
    if git["tracked_diff_sha256"] != EMPTY_SHA256:
        raise SystemExit(f"publishable evidence contains a source delta: {run}")

    require_executable(environment.get("loadgen_binary"), "loadgen")
    tools = environment.get("tools")
    require_fields(tools, {"cargo", "rustc", "nginx", "haproxy"}, "tool metadata")
    for name in ("cargo", "rustc"):
        require_executable(tools[name], name)
        require_fields(tools[name], {"version", "version_arguments"}, name)
        if not isinstance(tools[name]["version"], str) or tools[name]["version_arguments"] != ["-Vv"]:
            raise SystemExit(f"{name} provenance was not captured with -Vv: {run}")

    implementation = run_metadata.get("implementation")
    measured = {
        "origin": ("nginx",),
        "oxiroute": (),
        "nginx": ("nginx",),
        "haproxy": ("haproxy",),
        "all": ("nginx", "haproxy"),
    }
    if implementation not in measured:
        raise SystemExit(f"unknown run implementation in {run}")
    if implementation in {"oxiroute", "all"}:
        require_executable(environment.get("oxiroute_binary"), "oxiroute")
    for name in measured[implementation]:
        require_executable(tools[name], name)


def evidence_source_files(run):
    required = [run / "run.json", run / "environment.json", run / "raw", run / "config", run / "logs"]
    missing = [str(path) for path in required if not path.exists()]
    if missing:
        raise SystemExit(f"run evidence is incomplete: {', '.join(missing)}")
    validate_run_provenance(run)

    run_metadata = json.loads((run / "run.json").read_text(encoding="utf-8"))
    implementation = run_metadata["implementation"]
    implementations = ("oxiroute", "nginx", "haproxy") if implementation == "all" else (implementation,)
    config_names = {
        "origin": ("nginx-origin.conf",),
        "oxiroute": ("nginx-origin.conf", "oxiroute.lua"),
        "nginx": ("nginx-origin.conf", "nginx-proxy.conf"),
        "haproxy": ("nginx-origin.conf", "haproxy.cfg"),
        "all": ("nginx-origin.conf", "oxiroute.lua", "nginx-proxy.conf", "haproxy.cfg"),
    }
    artifact_paths = []
    for current in implementations:
        artifact_paths.extend(
            [run / f"summary-{current}.json", run / "raw" / f"loadgen-{current}.json"]
        )
    artifact_paths.extend(run / "config" / name for name in config_names[implementation])
    log_names = ["origin-stdout.log", "origin-stderr.log"]
    for current in implementations:
        log_names.extend([f"loadgen-warmup-{current}.log", f"loadgen-{current}.log"])
        if current != "origin":
            log_names.extend([f"{current}-stdout.log", f"{current}-stderr.log"])
    artifact_paths.extend(run / "logs" / name for name in log_names)
    for path in artifact_paths:
        if path.is_symlink() or not path.is_file():
            raise SystemExit(f"run evidence is missing required artifact: {path}")
        if path.suffix != ".log" and path.stat().st_size == 0:
            raise SystemExit(f"run evidence contains an empty required artifact: {path}")

    files = [path for path in run.glob("*.json") if path.is_file()]
    for directory in (run / "raw", run / "config", run / "logs"):
        if directory.is_symlink():
            raise SystemExit(f"run evidence must not contain symlinks: {directory}")
        for path in directory.rglob("*"):
            if path.is_symlink():
                raise SystemExit(f"run evidence must not contain symlinks: {path}")
            if path.is_file():
                files.append(path)
    for directory in (run / "raw", run / "config", run / "logs"):
        if not any(is_below(path, directory) for path in files):
            raise SystemExit(f"run evidence has no files below {directory}")
    for path in files:
        if path.is_symlink():
            raise SystemExit(f"run evidence must not contain symlinks: {path}")
    return sorted(files, key=lambda path: path.relative_to(run).as_posix())


def evidence_file_record(root, path):
    return {
        "path": path.relative_to(root).as_posix(),
        "bytes": path.stat().st_size,
        "sha256": sha256_file(path),
    }


def publish_evidence(args):
    supplied_report_path = pathlib.Path(args.report).absolute()
    report_path = supplied_report_path.resolve()
    require_publishable_report_path(supplied_report_path, report_path)
    report_source = report_path.read_bytes()
    report = json.loads(report_source)
    if "evidence" in report:
        raise SystemExit("report already contains an evidence reference")

    runs = [pathlib.Path(path).resolve() for path in args.runs]
    if any(run.parent != GENERATED_RUNS_ROOT for run in runs):
        raise SystemExit(f"runs must be direct children of {GENERATED_RUNS_ROOT}")
    if len({run.name for run in runs}) != len(runs):
        raise SystemExit("run evidence directory names must be unique")
    expected_run_ids = report_run_ids(report)
    if not expected_run_ids:
        raise SystemExit("publishable reports must contain nonempty run_ids")
    actual_run_ids = {run.name for run in runs}
    if expected_run_ids != actual_run_ids:
        raise SystemExit(
            "report run IDs do not match supplied runs: "
            f"expected {sorted(expected_run_ids)}, got {sorted(actual_run_ids)}"
        )

    evidence_parent = report_path.parent / "evidence"
    destination = evidence_parent / report_path.stem
    if destination.exists():
        raise SystemExit(f"evidence destination already exists: {destination}")
    evidence_parent.mkdir(parents=True, exist_ok=True)
    temporary = pathlib.Path(tempfile.mkdtemp(prefix=f".{report_path.stem}.", dir=evidence_parent))
    try:
        for run in sorted(runs, key=lambda path: path.name):
            for source in evidence_source_files(run):
                relative = pathlib.Path("runs") / run.name / source.relative_to(run)
                target = temporary / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(source, target)

        payload = [
            evidence_file_record(temporary, path)
            for path in sorted(temporary.rglob("*"), key=lambda path: path.relative_to(temporary).as_posix())
            if path.is_file()
        ]
        manifest = {
            "schema": "oxiroute.benchmark-evidence.v2",
            "report": pathlib.Path(os.path.relpath(report_path, temporary)).as_posix(),
            "report_content": {
                "canonicalization": "json-sort-keys-v1-excluding-evidence",
                "sha256": hashlib.sha256(canonical_report_bytes(report)).hexdigest(),
            },
            "run_ids": sorted(actual_run_ids),
            "files": payload,
        }
        write_json(temporary / "manifest.json", manifest)
        checksum_files = [temporary / item["path"] for item in payload] + [temporary / "manifest.json"]
        checksums = "".join(
            f"{sha256_file(path)}  {path.relative_to(temporary).as_posix()}\n"
            for path in sorted(checksum_files, key=lambda path: path.relative_to(temporary).as_posix())
        )
        (temporary / "SHA256SUMS").write_text(checksums, encoding="utf-8")
        temporary.rename(destination)
    except BaseException:
        shutil.rmtree(temporary, ignore_errors=True)
        raise

    evidence_relative = destination.relative_to(report_path.parent).as_posix()
    report["evidence"] = {
        "status": "published",
        "root": evidence_relative,
        "manifest": f"{evidence_relative}/manifest.json",
        "checksums": f"{evidence_relative}/SHA256SUMS",
    }
    try:
        if report_path.read_bytes() != report_source:
            raise SystemExit(f"report changed during evidence publication: {report_path}")
        write_json_atomic(report_path, report)
    except BaseException:
        shutil.rmtree(destination, ignore_errors=True)
        raise
    print(f"evidence published: {destination}")


def validate_evidence(report_path, report, reports_root):
    reference = report["evidence"]
    required = {"status", "root", "manifest", "checksums"}
    if not isinstance(reference, dict) or set(reference) != required:
        raise SystemExit(f"invalid evidence reference in {report_path}")
    if reference["status"] != "published":
        raise SystemExit(f"invalid published evidence status in {report_path}")
    if not all(isinstance(reference[key], str) for key in required):
        raise SystemExit(f"evidence paths must be strings in {report_path}")

    root = (report_path.parent / reference["root"]).resolve()
    evidence_root = (reports_root / "evidence").resolve()
    if root.parent != evidence_root or root.name != report_path.stem:
        raise SystemExit(f"non-canonical evidence root in {report_path}")
    manifest_path = (report_path.parent / reference["manifest"]).resolve()
    checksums_path = (report_path.parent / reference["checksums"]).resolve()
    if manifest_path != root / "manifest.json" or checksums_path != root / "SHA256SUMS":
        raise SystemExit(f"non-canonical evidence paths in {report_path}")

    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if manifest.get("schema") != "oxiroute.benchmark-evidence.v2":
        raise SystemExit(f"unsupported evidence manifest schema: {manifest_path}")
    manifest_report = manifest.get("report")
    if not isinstance(manifest_report, str) or (root / manifest_report).resolve() != report_path:
        raise SystemExit(f"evidence manifest points to the wrong report: {manifest_path}")
    expected_run_ids = report_run_ids(report)
    if not expected_run_ids or sorted(expected_run_ids) != manifest.get("run_ids"):
        raise SystemExit(f"evidence run IDs do not match report: {manifest_path}")
    report_content = manifest.get("report_content")
    expected_report_content = {
        "canonicalization": "json-sort-keys-v1-excluding-evidence",
        "sha256": hashlib.sha256(canonical_report_bytes(report)).hexdigest(),
    }
    if report_content != expected_report_content:
        raise SystemExit(f"evidence manifest does not match report contents: {manifest_path}")

    for path in root.rglob("*"):
        if path.is_symlink():
            raise SystemExit(f"published evidence must not contain symlinks: {path}")
    payload_paths = sorted(
        (
            path
            for path in root.rglob("*")
            if path.is_file() and path not in {manifest_path, checksums_path}
        ),
        key=lambda path: path.relative_to(root).as_posix(),
    )
    records = [evidence_file_record(root, path) for path in payload_paths]
    if manifest.get("files") != records:
        raise SystemExit(f"evidence manifest does not match files: {manifest_path}")
    archived_runs = root / "runs"
    if not archived_runs.is_dir():
        raise SystemExit(f"evidence archive has no runs directory: {manifest_path}")
    archived_run_dirs = sorted(path for path in archived_runs.iterdir() if path.is_dir())
    archived_run_ids = [path.name for path in archived_run_dirs]
    if archived_run_ids != sorted(expected_run_ids):
        raise SystemExit(f"archived run directories do not match report: {manifest_path}")
    for run in archived_run_dirs:
        evidence_source_files(run)

    checksum_files = payload_paths + [manifest_path]
    expected_checksums = "".join(
        f"{sha256_file(path)}  {path.relative_to(root).as_posix()}\n"
        for path in sorted(checksum_files, key=lambda path: path.relative_to(root).as_posix())
    )
    if checksums_path.read_text(encoding="utf-8") != expected_checksums:
        raise SystemExit(f"evidence checksums do not match files: {checksums_path}")
    return root


def validate_historical_evidence(report_path, reference):
    required = {"schema", "status", "reason"}
    if report_path.name not in HISTORICAL_REPORTS:
        raise SystemExit(f"historical evidence exception is not allowed for {report_path}")
    if not isinstance(reference, dict) or set(reference) != required:
        raise SystemExit(f"invalid historical evidence marker in {report_path}")
    if (
        reference["schema"] != "oxiroute.benchmark-evidence-unavailable.v1"
        or reference["status"] != "historical_unavailable"
        or not isinstance(reference["reason"], str)
        or not reference["reason"].strip()
    ):
        raise SystemExit(f"invalid historical evidence marker in {report_path}")


def validate_reports(args):
    reports_root = pathlib.Path(args.reports).resolve()
    if reports_root != REPORTS_ROOT:
        raise SystemExit(f"reports root must be {REPORTS_ROOT}")
    referenced = set()
    historical = set()
    for report_path in sorted(reports_root.glob("*.json")):
        if report_path.is_symlink():
            raise SystemExit(f"report must not be a symlink: {report_path}")
        report = json.loads(report_path.read_text(encoding="utf-8"))
        reference = report.get("evidence")
        if not isinstance(reference, dict):
            raise SystemExit(f"report has no explicit evidence contract: {report_path}")
        if reference.get("status") == "published":
            referenced.add(validate_evidence(report_path.resolve(), report, reports_root))
        elif reference.get("status") == "historical_unavailable":
            validate_historical_evidence(report_path.resolve(), reference)
            historical.add(report_path.name)
        else:
            raise SystemExit(f"report has an unknown evidence status: {report_path}")

    missing_historical = sorted(HISTORICAL_REPORTS - historical)
    if missing_historical:
        raise SystemExit(f"historical reports lack evidence markers: {', '.join(missing_historical)}")

    evidence_root = reports_root / "evidence"
    published = {path.resolve() for path in evidence_root.iterdir() if path.is_dir()} if evidence_root.is_dir() else set()
    orphaned = sorted(str(path) for path in published - referenced)
    if orphaned:
        raise SystemExit(f"published evidence is not referenced by a report: {', '.join(orphaned)}")
    print(f"reports validated: {reports_root}")


def summarize_loadgen(args):
    report = json.loads(pathlib.Path(args.input).read_text(encoding="utf-8"))
    required = {
        "schema",
        "implementation",
        "load_generator",
        "protocol",
        "request_line",
        "connection_reuse",
        "connections",
        "requests_per_second",
        "requests",
        "elapsed_seconds",
        "transfer_bytes_per_second",
        "non_2xx_or_3xx",
        "failed_requests",
    }
    if required - report.keys():
        raise SystemExit("load generator output did not contain a complete result")
    if report["protocol"] != "HTTP/1.1" or not report["request_line"].endswith(" HTTP/1.1"):
        raise SystemExit("load generator did not report an HTTP/1.1 request")
    if report["connection_reuse"] != "keep-alive":
        raise SystemExit("load generator did not report keep-alive connection reuse")
    write_json(args.output, report)
    if report["non_2xx_or_3xx"] or report["failed_requests"]:
        raise SystemExit("load generator reported HTTP or request failures")


def skipped_lanes(args):
    manifest = json.loads(pathlib.Path(args.input).read_text(encoding="utf-8"))
    skipped = [lane for lane in manifest["lanes"] if lane["status"] == "skipped"]
    write_json(args.output, {"schema": manifest["schema"], "lanes": skipped})


def result_value(args):
    report = json.loads(pathlib.Path(args.input).read_text(encoding="utf-8"))
    print(report["requests_per_second"])


def build_parser():
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)

    command = commands.add_parser("render")
    command.add_argument("input")
    command.add_argument("output")
    command.add_argument("replacement", nargs="*")
    command.set_defaults(function=render)

    command = commands.add_parser("wait-http")
    command.add_argument("url")
    command.add_argument("timeout", type=float)
    command.add_argument("expected_bytes", type=int, nargs="?")
    command.set_defaults(function=wait_http)

    command = commands.add_parser("preflight")
    command.add_argument("output")
    command.add_argument("oxiroute_bin")
    command.add_argument("loadgen_bin")
    command.add_argument("origin_port", type=int)
    command.add_argument("proxy_port", type=int)
    command.add_argument("proxy_cpu", type=int)
    command.add_argument("origin_cpu", type=int)
    command.add_argument("load_cpu", type=int)
    command.add_argument("implementations", nargs="+")
    command.set_defaults(function=preflight)

    command = commands.add_parser("environment")
    command.add_argument("output")
    command.add_argument("repository")
    command.add_argument("oxiroute_bin")
    command.add_argument("loadgen_bin")
    command.set_defaults(function=environment)

    command = commands.add_parser("run-metadata")
    command.add_argument("output")
    command.add_argument("implementation")
    command.add_argument("origin_port", type=int)
    command.add_argument("proxy_port", type=int)
    command.add_argument("connections", type=int)
    command.add_argument("warmup_seconds", type=int)
    command.add_argument("duration_seconds", type=int)
    command.add_argument("proxy_cpu", type=int)
    command.add_argument("origin_cpu", type=int)
    command.add_argument("load_cpu", type=int)
    command.set_defaults(function=run_metadata)

    command = commands.add_parser("summarize-loadgen")
    command.add_argument("input")
    command.add_argument("output")
    command.set_defaults(function=summarize_loadgen)

    command = commands.add_parser("skipped-lanes")
    command.add_argument("input")
    command.add_argument("output")
    command.set_defaults(function=skipped_lanes)

    command = commands.add_parser("result-value")
    command.add_argument("input")
    command.set_defaults(function=result_value)

    command = commands.add_parser("publish-evidence")
    command.add_argument("report")
    command.add_argument("runs", nargs="+")
    command.set_defaults(function=publish_evidence)

    command = commands.add_parser("validate-reports")
    command.add_argument("reports")
    command.set_defaults(function=validate_reports)
    return parser


if __name__ == "__main__":
    parsed = build_parser().parse_args()
    parsed.function(parsed)
