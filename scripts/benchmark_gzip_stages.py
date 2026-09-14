#!/usr/bin/env python3
"""Measure gzip inflation and shared-window parsing on the complete input."""
import argparse
import datetime
import hashlib
import json
import pathlib
import platform
import statistics
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("file", type=pathlib.Path)
parser.add_argument("--repeats", type=int, default=3)
parser.add_argument("--window-mib", type=int, default=8)
parser.add_argument("--buffers", nargs="+", type=int, choices=[1, 2], default=[1])
parser.add_argument("--readers", nargs="+", type=int, default=[0, 1, 2, 4, 8])
parser.add_argument("--output", type=pathlib.Path, default=pathlib.Path("private/gzip-stages.json"))
args = parser.parse_args()
if min(args.repeats, args.window_mib) < 1:
    parser.error("repeats and window size must be positive")
if min(args.readers) < 0:
    parser.error("reader counts cannot be negative")
root = pathlib.Path(__file__).resolve().parents[1]
source = args.file.resolve(strict=True)
initial = source.stat()
subprocess.run(["cargo", "build", "--release", "--example", "gzip_stages"], cwd=root, check=True)
fingerprint = hashlib.sha256()
for path in sorted([*root.glob("src/**/*.rs"), root / "examples/gzip_stages.rs", root / "Cargo.toml", root / "Cargo.lock"]):
    fingerprint.update(str(path.relative_to(root)).encode() + b"\0" + path.read_bytes())
report = {
    "date_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "file": str(source), "compressed_bytes": initial.st_size,
    "mtime_ns": initial.st_mtime_ns, "platform": platform.platform(),
    "cpu": next(line.split(":", 1)[1].strip() for line in pathlib.Path("/proc/cpuinfo").read_text().splitlines() if line.startswith("model name")),
    "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
    "source_sha256": fingerprint.hexdigest(),
    "cache_policy": "No explicit warming/eviction; rotating configurations; cache uncontrolled.",
    "scope": "Full gzip input. One buffer serializes inflation and parsing; two overlap them. Readers=0 skips parsing (one buffer only). With one buffer parse time includes dispatch/wait; with two it sums the slowest worker time per batch, excluding coordinator dispatch/wait. Overlapping stage times must not be added.",
    "runs": [],
}
args.output.parent.mkdir(parents=True, exist_ok=True)
expected_bytes = expected_count = None
configurations = [(buffers, readers) for readers in args.readers for buffers in args.buffers if readers or buffers == 1]
if not configurations:
    parser.error("no valid configuration; two buffers require a parser reader")
for repeat in range(args.repeats):
    pivot = repeat % len(configurations)
    for buffers, readers in configurations[pivot:] + configurations[:pivot]:
        result = subprocess.run(
            ["/usr/bin/time", "-f", "STAGES_METRICS %e %U %S %M %I", str(root / "target/release/examples/gzip_stages"), str(source), str(readers), str(args.window_mib), str(buffers)],
            text=True, capture_output=True, check=True,
        )
        run = json.loads(result.stdout)
        metrics = next(line.split()[1:] for line in result.stderr.splitlines() if line.startswith("STAGES_METRICS "))
        run.update(repeat=repeat + 1, process_wall_seconds=float(metrics[0]), user_seconds=float(metrics[1]), system_seconds=float(metrics[2]), peak_rss_kib=int(metrics[3]), filesystem_inputs=int(metrics[4]))
        if expected_bytes is None:
            expected_bytes = run["decoded_bytes"]
        if readers and expected_count is None:
            expected_count = run["triples"]
        if run["decoded_bytes"] != expected_bytes or (readers and run["triples"] != expected_count):
            raise SystemExit(f"Count/size mismatch: {run}")
        current = source.stat()
        if (current.st_size, current.st_mtime_ns, current.st_ino, current.st_dev) != (initial.st_size, initial.st_mtime_ns, initial.st_ino, initial.st_dev):
            raise SystemExit("Input changed during measurement")
        report["runs"].append(run)
        args.output.write_text(json.dumps(report, indent=2) + "\n")
        print(f"round {repeat+1}, buffers {buffers}, readers {readers}: inflate {run['inflate_seconds']:.3f}s, parse {run['parse_seconds']:.3f}s, total {run['elapsed_seconds']:.3f}s, RSS {run['peak_rss_kib']/1024:.1f} MiB", flush=True)
print("buffers | readers | median inflate s | median parse s | median total s")
for buffers, readers in configurations:
    runs = [run for run in report["runs"] if run["readers"] == readers and run["buffer_count"] == buffers]
    print(buffers, readers, *(f"{statistics.median(run[key] for run in runs):.3f}" for key in ["inflate_seconds", "parse_seconds", "elapsed_seconds"]), sep=" | ")
