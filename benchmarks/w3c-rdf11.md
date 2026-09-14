# W3C RDF 1.1 measurements

All **210 syntax checks passed**: 41 positive and 29 negative cases, each tested
as plain input, serial gzip, and gzip with four parallel readers.

The timed workload uses only the 41 positive fixtures from
[W3C rdf-tests](https://github.com/w3c/rdf-tests/tree/369a90d1a60c021b746df2e411da0ff36258a758/rdf/rdf11/rdf-n-triples),
revision `369a90d1a60c021b746df2e411da0ff36258a758`. Each fixture is followed by LF, forming a 7,115-byte
block with 78 triples. Repeating it 37,729 times produces
**268,441,835 decoded bytes / 2,942,862 triples**.
`gzip -1 -n` produces **10,948,744 stored bytes**. Fixture
hashes and the generated input hash are recorded in [the raw results](w3c-rdf11.json).

This is a repeated conformance workload, not an independent large RDF graph.
The small vocabulary and high repetition affect cache behavior and compression.
Blank-node labels are repeated unchanged because this measures lexical parsing,
not RDF graph construction or identity assignment.

## Timings

3 repetitions per configuration, with rotating execution order and
1 MiB windows. Release build with thin LTO; rustc 1.98.1 (48a229cea 2026-09-01);
AMD Ryzen 5 PRO 4650U with Radeon Graphics. Every run validates syntax and counts borrowed quads without
retaining records. Gzip time includes inflation and archive validation;
throughput uses decoded bytes. Input generation and compilation are excluded.

| Input | Parser readers | Median seconds | Range seconds | Decoded MiB/s | Max RSS MiB |
|---|---:|---:|---:|---:|---:|
| plain | 1 | 0.510 | 0.504–0.515 | 502.2 | 3.4 |
| plain | 4 | 0.141 | 0.140–0.176 | 1,817.2 | 6.0 |
| gzip | 1 | 0.554 | 0.554–0.563 | 461.8 | 3.6 |
| gzip | 2 | 0.300 | 0.273–0.317 | 853.2 | 4.5 |
| gzip | 4 | 0.178 | 0.172–0.205 | 1,436.4 | 4.5 |

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
git -C /tmp/rdf-tests checkout 369a90d1a60c021b746df2e411da0ff36258a758
python3 scripts/benchmark_w3c.py /tmp/rdf-tests --size-mib 256 --window-mib 1 --repeats 3
```

The script checks the original fixtures separately, including EOF-sensitive and
negative syntax cases. Generated inputs live under ignored `target/bench-data`;
the public JSON contains only W3C provenance and measurements.
