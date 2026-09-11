#!/usr/bin/env python3
"""Time full-file RDF parsing without retaining records or pre-scanning input.

Requires Linux /usr/bin/time. Cache state is observed, never forcibly changed.
"""
import argparse
import datetime
import hashlib
import json
import pathlib
import platform
import re
import statistics
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("file", type=pathlib.Path)
parser.add_argument("--gzip-file", type=pathlib.Path, help="Compare with a gzip copy, using one sequential reader")
parser.add_argument("--threads", nargs="+", type=int)
parser.add_argument("--repeats", type=int, default=3)
parser.add_argument("--window-mib", type=int, default=8)
parser.add_argument("--output", type=pathlib.Path, default=pathlib.Path("benchmark-results.json"))
args = parser.parse_args()
if args.threads is None:
    args.threads = [1, 2, 4, 6, 8, 12]
if min(args.threads + [args.repeats, args.window_mib]) < 1:
    parser.error("thread counts, repeats, and window size must be positive")
root = pathlib.Path(__file__).resolve().parents[1]
source = args.file.resolve(strict=True)
initial = source.stat()
if source.suffix == ".gz" and args.gzip_file:
    parser.error("--gzip-file requires a plain primary input")
configurations = [(source, threads) for threads in args.threads]
if args.gzip_file:
    configurations.append((args.gzip_file.resolve(strict=True), 1))
initial_stats = {path: path.stat() for path, _ in configurations}
subprocess.run(["cargo", "build", "--release", "--example", "count"], cwd=root, check=True)
fingerprint = hashlib.sha256()
for path in sorted([*root.glob("src/**/*.rs"), root / "examples/count.rs", root / "Cargo.toml", root / "Cargo.lock"]):
    fingerprint.update(str(path.relative_to(root)).encode() + b"\0" + path.read_bytes())
report = {
    "date_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "file": str(source),
    "file_bytes": initial.st_size,
    "file_mtime_ns": initial.st_mtime_ns,
    "platform": platform.platform(),
    "cpu": next((line.split(":", 1)[1].strip() for line in pathlib.Path("/proc/cpuinfo").read_text().splitlines() if line.startswith("model name")), "unknown"),
    "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
    "source_sha256": fingerprint.hexdigest(),
    "window_mib_per_reader": args.window_mib,
    "cache_policy": "No cache pre-scan or flushing; runs are sequential, cache state uncontrolled.",
    "scope": "Full-file reading (including inflation for gzip), N-Triples syntax validation, tokenization and counting; no RDF term materialization/indexing.",
    "inputs": [{"file": str(path), "stored_bytes": stat.st_size, "mtime_ns": stat.st_mtime_ns} for path, stat in initial_stats.items()],
    "runs": [],
}
args.output.parent.mkdir(parents=True, exist_ok=True)

def save():
    args.output.write_text(json.dumps(report, indent=2) + "\n")

save()
expected_count = None
expected_decoded_bytes = None
for repeat in range(1, args.repeats + 1):
    # Rotate configurations between rounds to reduce a fixed first-run advantage.
    pivot = (repeat - 1) % len(configurations)
    order = configurations[pivot:] + configurations[:pivot]
    for source, threads in order:
        command = ["/usr/bin/time", "-f", "S_PARSER_METRICS %e %U %S %M %F %R %I",
                   str(root / "target/release/examples/count"), str(source), str(threads), str(args.window_mib)]
        result = subprocess.run(command, text=True, capture_output=True)
        match = re.search(r"(\d+) triples in ([\d.]+)s; ([\d.]+) MiB/s; ([\d.]+) M triples/s", result.stdout)
        metrics = re.search(r"S_PARSER_METRICS ([\d.]+) ([\d.]+) ([\d.]+) (\d+) (\d+) (\d+) (\d+)", result.stderr)
        if result.returncode or not match or not metrics:
            report["error"] = {"returncode": result.returncode, "stdout": result.stdout, "stderr": result.stderr}
            save()
            raise SystemExit(f"Benchmark failed: {result.stderr}")
        count = int(match[1])
        if expected_count is None:
            expected_count = count
        if count != expected_count:
            raise SystemExit(f"Record count changed: {expected_count} -> {count}")
        decoded = re.search(r"(\d+) decoded bytes", result.stdout)
        if not decoded:
            raise SystemExit(f"Missing decoded byte count: {result.stdout}")
        decoded_bytes = int(decoded[1])
        if expected_decoded_bytes is None:
            expected_decoded_bytes = decoded_bytes
        if decoded_bytes != expected_decoded_bytes:
            raise SystemExit(f"Decoded size changed: {expected_decoded_bytes} -> {decoded_bytes}")
        current = source.stat()
        initial = initial_stats[source]
        if (current.st_size, current.st_mtime_ns, current.st_ino, current.st_dev) != (initial.st_size, initial.st_mtime_ns, initial.st_ino, initial.st_dev):
            raise SystemExit("Input file changed during benchmark")
        run = {
            "format": "gzip" if source.suffix == ".gz" else "plain", "file": str(source),
            "decoded_bytes": decoded_bytes,
            "repeat": repeat, "threads": threads, "triples": count,
            "parse_seconds": float(match[2]), "mib_per_second": float(match[3]),
            "million_triples_per_second": float(match[4]),
            "process_wall_seconds": float(metrics[1]), "user_seconds": float(metrics[2]),
            "system_seconds": float(metrics[3]), "peak_rss_kib": int(metrics[4]),
            "major_faults": int(metrics[5]), "minor_faults": int(metrics[6]),
            "filesystem_inputs": int(metrics[7]),
        }
        report["runs"].append(run)
        save()
        print(f"round {repeat}, {run['format']}, {threads:2} readers: {run['parse_seconds']:.3f}s, {run['mib_per_second']:.1f} MiB/s, RSS {run['peak_rss_kib'] / 1024:.1f} MiB, inputs {run['filesystem_inputs']}", flush=True)
print("\nformat | readers | best s | median s | worst s | max RSS MiB")
for source, threads in configurations:
    runs = [run for run in report["runs"] if run["threads"] == threads and run["file"] == str(source)]
    times = [run["parse_seconds"] for run in runs]
    print(f"{runs[0]['format']} | {threads:7} | {min(times):6.3f} | {statistics.median(times):8.3f} | {max(times):7.3f} | {max(run['peak_rss_kib'] for run in runs) / 1024:.1f}")
print(f"\nRaw results: {args.output}")
