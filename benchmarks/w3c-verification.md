# W3C benchmark verification

No functional regression was found after the history cleanup. The parser source,
counting examples, Cargo manifest and lockfile match the recorded source
fingerprint. Corpus revision, fixture hashes, generated input SHA-256, compiler,
platform, window size, and workload dimensions also match the previous report.

- **37 release tests passed**, including SIMD differential checks, window/range
  boundaries, malformed input, bounded allocation, and gzip pipeline tests.
- **7 doctests passed**, including borrowing restrictions.
- **210 W3C syntax checks passed** across plain, serial gzip, and parallel gzip.
- All **15 timed runs** returned **2,942,862 triples / 268,441,835 decoded bytes**.

Previous results come from `76ce67a9646883d439c954c6b0f05c8049c040f5:benchmarks/w3c-rdf11.json`.
The rerun uses the same repeated W3C positive-fixture workload, three repetitions
per configuration, rotating order, and 1 MiB windows. Builds and tests finished
before timed runs. Input generation is excluded from timing.

| Input | Readers | Previous median | Rerun median | Change |
|---|---:|---:|---:|---:|
| plain | 1 | 0.510 s | 0.505 s | -1.0% |
| plain | 4 | 0.141 s | 0.148 s | +5.0% |
| gzip | 1 | 0.554 s | 0.553 s | -0.2% |
| gzip | 2 | 0.300 s | 0.285 s | -5.0% |
| gzip | 4 | 0.178 s | 0.185 s | +3.9% |

Positive change means more elapsed time. Changes are approximately -5% to +5%,
and the observed elapsed-time ranges overlap for every configuration. These
short runs do not establish statistical equivalence, but show no clear timing
regression. Peak RSS remains comparable: about 3.4 MiB for one plain reader,
6.0 MiB for four plain readers, 3.6 MiB for serial gzip, and 4.5–4.6 MiB for
parallel gzip. Cache state is not explicitly controlled; this remains a repeated
conformance workload rather than a representative production graph.

The [current W3C report](w3c-rdf11.md) contains full provenance and reproduction
commands. [Verification data](w3c-verification.json) preserves both sets of raw
runs. Parser implementation files were not modified during verification.
