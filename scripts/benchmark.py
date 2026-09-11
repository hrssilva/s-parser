#!/usr/bin/env python3
"""Reproduce the synthetic throughput/RSS check (Linux, requires /usr/bin/time)."""
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
subprocess.run(["cargo", "build", "--release", "--example", "count"], cwd=root, check=True)
line = b'<http://example.org/subject> <http://example.org/predicate> "a representative literal"@en .\n'
block = line * 10000
with tempfile.TemporaryDirectory(prefix="s-parser-benchmark-") as directory:
    path = pathlib.Path(directory) / "input.nt"
    for blocks in (20, 200):
        with path.open("wb") as output:
            for _ in range(blocks):
                output.write(block)
        for threads in (1, 4):
            for run in range(1, 4):
                result = subprocess.run(
                    ["/usr/bin/time", "-f", "peak_RSS_KiB=%M",
                     str(root / "target/release/examples/count"), str(path), str(threads), "1"],
                    capture_output=True, text=True, check=True,
                )
                print(f"{blocks * 10000} records, run {run}: {result.stdout.strip()}; {result.stderr.strip()}", flush=True)
