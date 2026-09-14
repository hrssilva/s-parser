#!/usr/bin/env python3
"""Build and measure a repeated workload exclusively from W3C positive fixtures.

The original 70-case manifest is validated separately with check_w3c.py. Public
reports contain corpus provenance and relative paths, not local account paths.
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
parser.add_argument("checkout", type=pathlib.Path)
parser.add_argument("--size-mib", type=int, default=256)
parser.add_argument("--window-mib", type=int, default=1)
parser.add_argument("--repeats", type=int, default=3)
args = parser.parse_args()
if min(args.size_mib, args.window_mib, args.repeats) < 1:
    parser.error("sizes and repeats must be positive")
root = pathlib.Path(__file__).resolve().parents[1]
checkout = args.checkout.resolve(strict=True)
relative_corpus = "rdf/rdf11/rdf-n-triples"
corpus = checkout / relative_corpus
revision = subprocess.check_output(["git", "-C", str(checkout), "rev-parse", "HEAD"], text=True).strip()
if subprocess.check_output(["git", "-C", str(checkout), "status", "--porcelain", "--untracked-files=no", "--", relative_corpus]):
    raise SystemExit("W3C fixtures must match the recorded corpus revision")
manifest_bytes = (corpus / "manifest.ttl").read_bytes()
cases = re.findall(
    r"rdf:type rdft:TestNTriples(Positive|Negative)Syntax\s*;.*?mf:action\s+<([^>]+)>",
    manifest_bytes.decode(), re.S,
)
if len(cases) != 70 or sum(kind == "Positive" for kind, _ in cases) != 41:
    raise SystemExit("Unexpected W3C manifest; inspect changes before publishing results")
subprocess.run(["python3", str(root / "scripts/check_w3c.py"), str(checkout)], cwd=root, check=True)
subprocess.run(["cargo", "build", "--release", "--example", "gzip_stages"], cwd=root, check=True)

fixtures = []
block = bytearray()
triples_per_block = 0
for kind, name in cases:
    if kind != "Positive":
        continue
    contents = (corpus / name).read_bytes()
    # N-Triples physical lines use CR/LF; other control bytes can be literals.
    expected = sum(bool(line.strip(b" \t")) and not line.lstrip(b" \t").startswith(b"#")
                   for line in re.split(rb"[\r\n]", contents))
    result = subprocess.check_output([str(root / "target/release/examples/count"), str(corpus / name), "1", "1"], text=True)
    if int(result.split()[0]) != expected:
        raise SystemExit(f"Physical-line count disagrees with parser for {name}")
    triples_per_block += expected
    block.extend(contents)
    block.append(10)  # Keep EOF comments/records separate from the next fixture.
    fixtures.append({"file": name, "bytes": len(contents), "triples": expected,
                     "sha256": hashlib.sha256(contents).hexdigest()})
copies = (args.size_mib * 1024 * 1024 + len(block) - 1) // len(block)
data_dir = root / "target/bench-data"
data_dir.mkdir(parents=True, exist_ok=True)
plain = data_dir / "w3c-rdf11-positive.nt"
compressed = data_dir / "w3c-rdf11-positive.nt.gz"
digest = hashlib.sha256()
with plain.open("wb") as output:
    for _ in range(copies):
        output.write(block)
        digest.update(block)
with compressed.open("wb") as output:
    subprocess.run(["gzip", "-1", "-n", "-c", str(plain)], stdout=output, check=True)
expected_bytes = len(block) * copies
expected_triples = triples_per_block * copies
fingerprint = hashlib.sha256()
for path in sorted([*root.glob("src/**/*.rs"), root / "examples/count.rs", root / "examples/gzip_stages.rs", root / "Cargo.toml", root / "Cargo.lock"]):
    fingerprint.update(str(path.relative_to(root)).encode() + b"\0" + path.read_bytes())
report = {
    "date_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "corpus_url": "https://github.com/w3c/rdf-tests", "corpus_revision": revision,
    "manifest": relative_corpus + "/manifest.ttl",
    "manifest_sha256": hashlib.sha256(manifest_bytes).hexdigest(),
    "syntax_checks": {"positive": 41, "negative": 29, "modes": ["plain", "serial gzip", "parallel gzip (4 readers)"], "passed": 210},
    "generation": "Manifest-order positive files, LF appended after each file, complete block repeated; gzip -1 -n. Blank-node labels unchanged; lexical counting only.",
    "fixtures": fixtures, "block_bytes": len(block), "block_triples": triples_per_block,
    "block_copies": copies, "decoded_bytes": expected_bytes, "triples": expected_triples,
    "input_sha256": digest.hexdigest(), "compressed_bytes": compressed.stat().st_size,
    "window_mib": args.window_mib, "source_sha256": fingerprint.hexdigest(),
    "cpu": next(line.split(":", 1)[1].strip() for line in pathlib.Path("/proc/cpuinfo").read_text().splitlines() if line.startswith("model name")),
    "platform": platform.platform(),
    "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
    "cache_policy": "Inputs created immediately before measurement; no eviction. Rotating modes, no concurrent builds/tests. Repeated fixtures and warm file cache limit generalization.",
    "runs": [],
}
json_path = root / "benchmarks/w3c-rdf11.json"
json_path.parent.mkdir(exist_ok=True)
configs = [("plain", 1), ("plain", 4), ("gzip", 1), ("gzip", 2), ("gzip", 4)]
initial_stats = {path: path.stat() for path in (plain, compressed)}
for repeat in range(args.repeats):
    pivot = repeat % len(configs)
    for kind, readers in configs[pivot:] + configs[:pivot]:
        source = plain if kind == "plain" else compressed
        result = subprocess.run(
            ["/usr/bin/time", "-f", "W3C_METRICS %e %U %S %M %I",
             str(root / "target/release/examples/count"), str(source), str(readers), str(args.window_mib)],
            capture_output=True, text=True, check=True,
        )
        values = re.search(r"(\d+) triples in ([\d.]+)s; ([\d.]+) MiB/s; ([\d.]+) M triples/s", result.stdout)
        decoded = re.search(r"(\d+) decoded bytes", result.stdout)
        if not values or not decoded or int(values[1]) != expected_triples or int(decoded[1]) != expected_bytes:
            raise SystemExit("Generated workload count/size mismatch")
        current, initial = source.stat(), initial_stats[source]
        if (current.st_size, current.st_mtime_ns, current.st_ino) != (initial.st_size, initial.st_mtime_ns, initial.st_ino):
            raise SystemExit("Benchmark input changed")
        metrics = next(line.split()[1:] for line in result.stderr.splitlines() if line.startswith("W3C_METRICS "))
        run = {"repeat": repeat + 1, "format": kind, "readers": readers,
               "parse_seconds": float(values[2]), "decoded_mib_per_second": float(values[3]),
               "triples": expected_triples, "decoded_bytes": expected_bytes,
               "process_wall_seconds": float(metrics[0]), "user_seconds": float(metrics[1]),
               "system_seconds": float(metrics[2]), "peak_rss_kib": int(metrics[3]), "filesystem_inputs": int(metrics[4])}
        report["runs"].append(run)
        json_path.write_text(json.dumps(report, indent=2) + "\n")
        print(f"round {repeat + 1}: {kind}, {readers} readers, {run['parse_seconds']:.3f}s, RSS {run['peak_rss_kib']/1024:.1f} MiB", flush=True)

rows = ["| Input | Parser readers | Median seconds | Range seconds | Decoded MiB/s | Max RSS MiB |",
        "|---|---:|---:|---:|---:|---:|"]
for kind, readers in configs:
    runs = [run for run in report["runs"] if (run["format"], run["readers"]) == (kind, readers)]
    times = [run["parse_seconds"] for run in runs]
    rows.append(f"| {kind} | {readers} | {statistics.median(times):.3f} | {min(times):.3f}–{max(times):.3f} | {statistics.median(run['decoded_mib_per_second'] for run in runs):,.1f} | {max(run['peak_rss_kib'] for run in runs)/1024:.1f} |")
text = f"""# W3C RDF 1.1 measurements

All **210 syntax checks passed**: 41 positive and 29 negative cases, each tested
as plain input, serial gzip, and gzip with four parallel readers.

The timed workload uses only the 41 positive fixtures from
[W3C rdf-tests](https://github.com/w3c/rdf-tests/tree/{revision}/{relative_corpus}),
revision `{revision}`. Each fixture is followed by LF, forming a {len(block):,}-byte
block with {triples_per_block} triples. Repeating it {copies:,} times produces
**{expected_bytes:,} decoded bytes / {expected_triples:,} triples**.
`gzip -1 -n` produces **{compressed.stat().st_size:,} stored bytes**. Fixture
hashes and the generated input hash are recorded in [the raw results](w3c-rdf11.json).

This is a repeated conformance workload, not an independent large RDF graph.
The small vocabulary and high repetition affect cache behavior and compression.
Blank-node labels are repeated unchanged because this measures lexical parsing,
not RDF graph construction or identity assignment.

## Timings

{args.repeats} repetitions per configuration, with rotating execution order and
{args.window_mib} MiB windows. Release build with thin LTO; {report['rustc']};
{report['cpu']}. Every run validates syntax and counts borrowed quads without
retaining records. Gzip time includes inflation and archive validation;
throughput uses decoded bytes. Input generation and compilation are excluded.

""" + "\n".join(rows) + f"""

Plain input uses one bounded mmap per reader. Serial gzip uses one window;
parallel gzip uses **two shared windows total**, plus an inflater/input buffer
and worker overhead. Gzip reader counts exclude the caller's inflater thread.
RSS includes the process, runtime and resident stack pages; the OS file cache
is separate. The parser's record-size limit equals the configured window size.

Files are generated immediately before the runs; caches are not evicted.
Filesystem input counters are retained in the JSON. These numbers characterize
this repeated workload and should not be extrapolated to unrelated RDF datasets.

## Reproduce

```sh
git clone https://github.com/w3c/rdf-tests.git /tmp/rdf-tests
git -C /tmp/rdf-tests checkout {revision}
python3 scripts/benchmark_w3c.py /tmp/rdf-tests --size-mib {args.size_mib} --window-mib {args.window_mib} --repeats {args.repeats}
```

The script checks the original fixtures separately, including EOF-sensitive and
negative syntax cases. Generated inputs live under ignored `target/bench-data`;
the public JSON contains only W3C provenance and measurements.
"""
(root / "benchmarks/w3c-rdf11.md").write_text(text)
print("Public report: benchmarks/w3c-rdf11.md")
