# Eitri

A small, typed, embeddable scripting language for immediate execution.
The goal is sub-microsecond to a few microseconds from fresh source to result
for small modules. **No execution or performance result exists yet.**

The Rust library and CLI are bootstrap scaffolding. Every source, including
empty input, returns structured diagnostic `E0001`; nothing executes. The
public Rust surface is experimental, not a stable embedding API.

See [the specification](docs/README.md) and [implementation tasks](docs/tasks.md).

## Development

Rust 1.97.1 is pinned in `rust-toolchain.toml`. No external crates are needed.
The Cargo workspace includes `crates/*`. Packages inherit workspace metadata
and lints.

| Crate | Contents |
|---|---|
| `crates/eitri-base` | Lowest layer: `newtype_index!` typed ids, `IndexVec`, `IdRange`, `List`/`ListPool` pooled child lists, `Span`/`SourceId`, the Fx hasher, and the context-owned spelling `Interner` with predefined keyword symbols. No dependencies, no global state |
| `crates/eitri-diag` | The diagnostic model (severity, code, message, labelled byte spans, notes; faults and internal bugs included), a capped `Diagnostics` sink, and an excerpt renderer in the codespan-reporting layout (rounded box drawing or ASCII; rich, medium, or short). Depends only on `eitri-base`; every other layer reports through it |
| `crates/eitri` | The library, CLI runner, fixture tests, and benchmark fixtures. Re-exports `eitri-diag` as `eitri::diag` |

Layers have separate interfaces and explicit dependencies. The caller owns a
`Context`; submissions use `context.run(source)` with exclusive mutable access.
Each layer will borrow the relevant context state as it is implemented. No
global/thread-local runtime state or locks sit on the compiler/execution path;
independent contexts can run independently. The bootstrap context currently
has no state. Immutable constants and generated read-only tables are allowed.

```sh
cargo build --locked --workspace
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo test --locked -p eitri --test fixtures
cargo test --locked -p eitri-diag
cargo run --locked -p eitri -- crates/eitri/tests/fixtures/scalar.eit
```

CLI exits: `0` for success (not implemented), `1` for a source diagnostic,
`2` for invocation or file/UTF-8 errors. Diagnostics go to stderr; stdout is
reserved for future program output. Diagnostics are rendered with
source excerpts, in color only when stderr is a terminal and `NO_COLOR` is
unset.

CI builds and checks x86-64 Linux and AArch64 Linux/macOS. Windows execution
joins at T010. Cross-platform CI results are separate from local validation.

## Fixture harness

`crates/eitri/tests/fixtures.rs` discovers every `.eit` source under the
package's `tests/fixtures/` directory (recursively) with a matching `.expect`
file. Missing or orphan expectations, unknown entries, and an empty corpus
fail the test. Cases run with bounded parallelism and independent state;
every failure is reported with its expected and actual outcome.

The corpus is grouped as `ok/`, `fault/`, `reject/` (errors inside the
supported slice), and `unsupported/` (deliberately rejected neighbors). It is
the executable form of [the first slice](docs/implementation/first-slice.md).

A fixture may start with directive lines:

| Directive | Meaning |
|---|---|
| `//! host: test` | The script may import the harness host module `test` (below). Without it, importing `"test"` is an unresolved import |
| `//! budget: N` | Execution budget in steps (default 1,000,000) |
| `//! depth: N` | Maximum active script frames (default 4,096) |
| `//! pending: T010` | The task that makes this fixture pass. Until then it must still be **rejected**: the harness fails if it runs, succeeds, or already matches (remove the marker then) |

The harness host module `test`:

```eit
pub fn emit(value: i64) -> void     // records the effect
pub fn check(value: bool) -> bool   // records the effect, returns value
pub fn id(value: i64) -> i64        // records the effect, returns value
pub fn fail() -> void               // faults with "requested by fixture"
```

`.expect` holds zero or more `effect: test.f(args)` lines in call order,
then exactly one outcome:

```text
ok
error[E0301] 18..22: type mismatch: expected `i64`, found `bool`
fault: integer overflow
  at bump 69..74
  at <module> 98..129
```

A rejection lists one or more diagnostics and never has effects. A fault is
followed by its trace, innermost first; host functions appear by qualified
name without a range. Spans are half-open UTF-8 byte offsets into the whole
`.eit` file, directives included. The harness validates every expectation's
shape and spans even while it is pending. Edit expectations explicitly and
review the diff; tests never rewrite them.

Remaining T045 work: execution adapters for effects, faults, and budgets
(T010), disassembly snapshots, and the T004 oracle and tier adapters. Do not
simulate successful execution to test them.

Benchmark sources live separately under `crates/eitri/benches/fixtures/`. The benchmark
command, raw-sample reports, stress/fuzz commands, and oracle comparisons
will be documented when implemented; none are available at bootstrap.

## Safety

`unsafe` is allowed with an explained performance benefit: identify the cost
removed, the safe alternative, and supporting measurements or a planned
experiment. Keep each block narrow and document its invariants and why they
hold in a `SAFETY` comment. Workspace lints deny undocumented unsafe blocks
and implicit unsafe operations inside unsafe functions. Add appropriate Miri
checks for supported frame/slot/handler code. C API work
must add AddressSanitizer and UndefinedBehaviorSanitizer coverage at T030.
There is no unsafe code, C API, or VM memory model to validate yet.
