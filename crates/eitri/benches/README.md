# Benchmarks

`source_to_result.rs` (T011) measures fresh source text to a checked result
for each workload in `fixtures/`: compile and first execution, timed
separately, in a reused initialized runtime and in a fresh runtime per
sample. `lua/` runs the equivalent `fixtures/*.lua` programs on Lua 5.4 by
the same method.

```sh
cargo bench -p eitri --bench source_to_result [samples]
crates/eitri/benches/lua/run.sh [samples]   # needs Lua 5.4 (brew install lua@5.4)
```

Each workload ends with one host call carrying its result (`bench.emit` in
Eitri, `emit` in Lua), which both harnesses check on every run against the
answer listed in the harness. Raw samples are written to
`target/bench-results/`. Results and method are recorded in the
[research ledger](../../../docs/implementation/vm-research.md).

| Workload | Exercises |
|---|---|
| `module` | A small setup-script-shaped module: definitions, top-level bindings, a few calls |
| `arithmetic` | A 1,000-iteration loop of checked and wrapping integer arithmetic |
| `branching` | `if`/`else if` chains, `&&`/`||`, `!`, and an `if` value in a loop |
| `calls` | Recursive script calls: `fib(15)` |
| `host_calls` | 1,000 calls to a scalar host function |

## CPU profiles and compile stages

`path_profile` uses the same five fixtures and answers, without the T011
counting allocator. It changes no compiler/VM behavior. Build with symbols:

```sh
CARGO_PROFILE_RELEASE_DEBUG=2 cargo build --release -p eitri --example path_profile
xcrun dsymutil target/release/examples/path_profile  # macOS symbolication
mkdir -p target/bench-results
```

Sampling modes take a workload name and duration in seconds (default 5).
`vm` compiles once, then executes the owned module repeatedly, checking the
emitted result each time. `compile` validates the workload once, then repeatedly
compiles fresh artifacts and disposes of them. `lower` validates once, keeps
the checked input, then repeatedly lowers and disposes of fresh artifacts.
These are CPU sampling targets, not fresh-source latency measurements.
The loops check elapsed time every 256 operations. File I/O and registration
precede the loops. No source cache or native tier is involved.

```sh
target/release/examples/path_profile vm calls 8 > target/bench-results/sample-calls-run.txt &
profile_pid=$!
sleep 1  # exclude startup from sampling
sample "$profile_pid" 5 -file target/bench-results/sample-calls.txt
wait "$profile_pid"
```

Repeat with `vm arithmetic`, `vm branching`, `compile module`, and `lower
module`, using distinct output filenames. Sampling can require profiling
permissions on macOS. For sampled PCs and assembly inspection:

```sh
xctrace record --template 'Time Profiler' --output target/bench-results/vm-calls.trace \
  --time-limit 6s --no-prompt --launch -- "$PWD/target/release/examples/path_profile" vm calls 5
xctrace export --input target/bench-results/vm-calls.trace \
  --xpath '/trace-toc/run[@number="1"]/data/table[@schema="time-sample"]' \
  --output target/bench-results/vm-calls.xml
xcrun otool -tvV target/release/examples/path_profile > target/bench-results/path_profile.asm
```

`stages` instead takes a sample count (default 20,001). After 1,000 warm-up
runs, it individually times lex, parse, check, lower (including owned artifact
construction), and artifact disposal. It executes and validates every artifact
outside those intervals and writes `target/bench-results/stages-NAME.csv`.
Interner reset and diagnostics creation/disposal are outside these intervals;
verification is omitted as in release `Context::compile`. Stage sums are a
phase breakdown, not a replacement for the full T011 measurement. Clock
quantization (~42 ns here) matters for tiny stages; no overhead is subtracted.

```sh
for workload in module calls arithmetic branching host_calls; do
  target/release/examples/path_profile stages "$workload" 20001
done
```

Compute p50/p99 from the individual CSV rows, never from batch averages.
Use the original benchmark separately for allocations, retained scratch,
first execution, total latency, and Lua comparison. See the
[profile findings](../../../docs/implementation/vm-research.md#e4e5-and-t011-path-profiles--2026-11-07).
