# Research prototypes

These files record the experiments before SIMD integration. The SSE2 IRI path is
now enabled in the production parser, with differential checks in its regular
test suite. The advice-only change remains experimental.

The historical recipe below requires the original scalar source from before
integration; do not apply `sse2-iri.patch` to the current implementation. To
compare the current implementation with scalar scanning, use an isolated copy
and change the `NtTokenizer` trait implementation's `decode::<true>(bytes, eof)`
call to `decode::<false>(bytes, eof)`. Build and save both variants before timing,
using `replay.rs` or the normal counting example. No public configuration or
runtime branch is needed for this experiment.

- `sse2-iri.patch`: skip verified ordinary ASCII IRI bytes in 16-byte blocks on
  x86_64, preserving the original scanner for all special bytes and tails.
- `sequential-advice.patch`: add `memmap2::Advice::Sequential` to each new mapping,
  independently of the SIMD change.
- `replay.rs`: example that loads three fixed 8 MiB samples from a file and times
  32 in-memory parses per sample. File loading is outside the timed region.
- `differential.rs`: example that compares a patched scanner with the original
  on byte mutations and every prefix of sample records.

In an isolated copy, build and save the baseline first:

```sh
cp benchmarks/prototypes/replay.rs examples/replay.rs
cargo build --release --examples
cp target/release/examples/count /tmp/s-parser-count-baseline
cp target/release/examples/replay /tmp/s-parser-replay-baseline
mkdir -p examples/reference
cp src/tokenizer/nt_tokenizer.rs examples/reference/mod.rs
```

Then apply the SIMD patch:

```sh
git apply benchmarks/prototypes/sse2-iri.patch
cp benchmarks/prototypes/differential.rs examples/differential.rs
cargo test --release
cargo run --release --example differential
cargo build --release --examples
cp target/release/examples/count /tmp/s-parser-count-sse2
cp target/release/examples/replay /tmp/s-parser-replay-sse2
python3 scripts/check_w3c.py /path/to/rdf-tests
```

Apply the advice-only patch to a separate baseline copy, not on top of the SIMD
patch, for an isolated comparison:

```sh
git apply benchmarks/prototypes/sequential-advice.patch
cargo build --release --example count
cp target/release/examples/count /tmp/s-parser-count-sequential
```

Run the saved executables sequentially, with at least three repetitions and
rotating variant order. Do not compile or run other benchmarks concurrently:

```sh
/tmp/s-parser-replay-baseline /path/to/input.nt
/tmp/s-parser-replay-sse2 /path/to/input.nt
/usr/bin/time -v /tmp/s-parser-count-baseline /path/to/input.nt 1 8
/usr/bin/time -v /tmp/s-parser-count-sse2 /path/to/input.nt 1 8
/usr/bin/time -v /tmp/s-parser-count-sequential /path/to/input.nt 1 8
```

Repeat full-file comparisons with eight readers. Window comparisons use the
baseline and the same reader count, varying the final argument between 1, 8 and
32 MiB. Keep the input immutable throughout all mapping lifetimes.
