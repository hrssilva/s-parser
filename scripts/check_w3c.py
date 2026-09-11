#!/usr/bin/env python3
"""Run RDF 1.1 N-Triples syntax tests from a local w3c/rdf-tests checkout.

Usage: python3 scripts/check_w3c.py /path/to/rdf-tests
The checkout is external; this script never downloads data.
"""
import pathlib
import re
import subprocess
import sys

root = pathlib.Path(__file__).resolve().parents[1]
corpus = pathlib.Path(sys.argv[1]).resolve() / "rdf/rdf11/rdf-n-triples"
manifest = (corpus / "manifest.ttl").read_text()
# Match the fixed layout of this particular W3C manifest, not arbitrary Turtle.
tests = re.findall(
    r"rdf:type rdft:TestNTriples(Positive|Negative)Syntax\s*;.*?mf:action\s+<([^>]+)>",
    manifest,
    re.S,
)
if len(tests) != 70:
    raise SystemExit(f"Expected 70 RDF 1.1 syntax tests, found {len(tests)}; inspect manifest changes")
subprocess.run(["cargo", "build", "--release", "--example", "count"], cwd=root, check=True)
failed = []
copies = root / "target/w3c-gzip"
copies.mkdir(parents=True, exist_ok=True)
for kind, name in tests:
    plain = corpus / name
    compressed = plain.with_name(plain.name + ".gz")
    if not compressed.exists():
        compressed = copies / (plain.name + ".gz")
        with compressed.open("wb") as output:
            subprocess.run(["gzip", "-n", "-c", str(plain)], stdout=output, check=True)
    plain_count = None
    for source, readers in ((plain, 1), (compressed, 1), (compressed, 4)):
        result = subprocess.run(
            [str(root / "target/release/examples/count"), str(source), str(readers), "1"],
            text=True, capture_output=True,
        )
        passed = result.returncode == 0 if kind == "Positive" else (
            result.returncode == 1 and result.stderr.startswith("Error: Syntax {")
        )
        if passed and kind == "Positive":
            count = int(result.stdout.split()[0])
            if source == plain:
                plain_count = count
            else:
                passed = count == plain_count
        if not passed:
            failed.append(source.name)
            print(f"FAIL {kind} {source.name}, {readers} readers: {result.stderr.strip()}")
print(f"{3 * len(tests) - len(failed)}/{3 * len(tests)} W3C RDF 1.1 N-Triples syntax checks passed (plain + serial gzip + alternating-window gzip)")
raise SystemExit(bool(failed))
