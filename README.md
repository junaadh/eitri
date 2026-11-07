<div align="center">

# ⚒️ Eitri

**A small, typed, embeddable scripting language that goes from fresh source text to a result in microseconds.**

*Named for the dwarf smith who forged Mjölnir. The secret was never letting the bellows stop.*

[Quick start](#quick-start) · [Benchmarks](#benchmarks) · [How it works](#how-it-works) · [Development](#development) · [Specification](docs/README.md)

</div>

---

Most scripting languages make you choose: start fast, or run fast. Eitri
refuses to choose. It is statically typed, so it never checks a type at run
time; it compiles every script before running it, so nothing runs if
anything is wrong; and it does both in less time than Lua takes to load the
same file.

```eit
import game from "game"

fn fib(n: i64) -> i64 {
    if n < 2 {
        return n
    }
    return fib(n - 1) + fib(n - 2)
}

var i = 0
while i < 10 {
    game.emit(fib(i))
    i += 1
}
```

- **🚀 Immediate.** No warmup, no JIT tier to wait for, no source cache. Every
  run is measured from fresh text, and still beats Lua 5.4 end to end.
- **🔒 Typed and checked up front.** One mistake anywhere and the whole script
  is rejected *before* any host function runs: no half-applied setup.
- **🧮 Honest arithmetic.** `i64` overflow and division by zero are faults,
  not surprises. Want wraparound? Say so: `&+`, `&-`, `&*`.
- **⏱️ Budgeted.** Every call and loop iteration draws from an execution budget,
  and call depth is capped, so a runaway script stops cleanly with a trace
  instead of hanging your host.
- **🧩 Embeddable.** The host owns everything: one `Context`, no globals, no
  locks, no reference counting. Independent contexts run independently.
- **🩺 Friendly diagnostics.** Errors and faults come with source excerpts,
  in the style of `rustc`.

> [!NOTE]
> Eitri is young. The first scalar slice (integers, booleans, functions,
> `if`, `while`, host calls) runs end to end; strings, collections,
> closures, and the rest of the [specification](docs/README.md) are on the
> [task list](docs/tasks.md). The Rust API is experimental.

## Quick start

Run a script with the CLI:

```sh
cargo run --release -p eitri -- path/to/script.eit
```

When something goes wrong, Eitri shows you where, and how you got there:

```text
fault: integer overflow
  ╭─ oops.eit:2:12
  │
2 │     return x + 1
  │            ^^^^^ in `bump`
  ·
5 │ let oops = bump(big)
  │            --------- in `<module>`
```

Exit codes: `0` success, `1` compile error, `2` usage or I/O error, `3`
runtime fault.

### Embedding

The host declares what scripts may call, then answers those calls by index:

```rust
use eitri::{Context, Host, RunError};

struct Game {
    emit: u32,
}

impl Host for Game {
    fn call(&mut self, index: u32, args: &[u64], _results: &mut [u64]) -> Result<(), String> {
        if index == self.emit {
            println!("{}", args[0] as i64);
        }
        Ok(())
    }
}

fn main() {
    let mut context = Context::new();
    context
        .register_host_module("game", "pub fn emit(value: i64) -> void\n")
        .expect("valid declarations");
    let emit = context.host_index("game", "emit").expect("declared above");

    match context.run(SCRIPT, &mut Game { emit }) {
        Ok(()) => {}
        Err(RunError::Compile(diagnostics)) => eprintln!("{} compile errors", diagnostics.len()),
        Err(RunError::Fault(fault)) => eprintln!("fault: {}", fault.message),
    }
}
```

Run it with the script above, and it prints `0 1 1 2 3 5 8 13 21 34`.
`Context::compile` and `Context::execute` split the two phases when you want
to run compiled code more than once. Budgets and depth are set with
`set_budget` and `set_depth`.

## Benchmarks

Every number below is **source text in, checked result out**: compile plus
first execution, timed together on every sample. No cached bytecode, no
warmed-up JIT. Each workload is written once in Eitri and once in Lua, and
both harnesses check the answer on every single run, so nothing can be
optimized away.

Medians of 20,001 individually timed runs in an already-created runtime,
same commit (`f4573b1`), same pinned compiler, Lua 5.4.9 built statically
with `-O2` and loaded with `luaL_loadbuffer` + `lua_pcall`.

### Apple M5 (MacBook Pro)

| Workload | Eitri | Lua 5.4 | |
|---|---:|---:|---|
| `module`: a 40-line setup script | **5.7 µs** | 9.0 µs | 🟢 1.57× faster |
| `arithmetic`: 1,000 checked-math iterations | **9.8 µs** | 17.1 µs | 🟢 1.74× faster |
| `branching`: `else if` chains, `&&`/`\|\|` | **19.9 µs** | 36.0 µs | 🟢 1.81× faster |
| `calls`: recursive `fib(15)`, 1,973 calls | **13.2 µs** | 16.0 µs | 🟢 1.21× faster |
| `host_calls`: 1,000 calls into Rust | **5.8 µs** | 11.2 µs | 🟢 1.95× faster |

### Apple M2 (MacBook Air)

| Workload | Eitri | Lua 5.4 | |
|---|---:|---:|---|
| `module` | **8.2 µs** | 12.2 µs | 🟢 1.49× faster |
| `arithmetic` | **15.0 µs** | 26.2 µs | 🟢 1.75× faster |
| `branching` | **36.7 µs** | 52.5 µs | 🟢 1.43× faster |
| `calls` | **21.5 µs** | 24.1 µs | 🟢 1.12× faster |
| `host_calls` | **8.9 µs** | 21.0 µs | 🟢 2.36× faster |

And this is while doing *more* work than Lua: every `+`, `-`, and `*` is
overflow-checked, and every call and backward branch draws from the
execution budget.

### Where the speed comes from

The interpreter is a **tail-call threaded** VM: one handler per opcode, each
ending by jumping straight into the next instruction's handler, with the
program counter, the instruction word, and the current frame pinned in
machine registers across the jumps. The older `match`-loop interpreter is
kept as a portable reference, and both must agree on every test. On the M2
it shows why the change mattered:

| Workload (M2) | `match` loop | Threaded | Speedup |
|---|---:|---:|---:|
| `arithmetic` | 30.1 µs | 15.0 µs | 2.0× |
| `branching` | 56.9 µs | 36.7 µs | 1.6× |
| `calls` | 30.6 µs | 21.5 µs | 1.4× |
| `host_calls` | 17.1 µs | 8.9 µs | 1.9× |

The `match` loop loses to Lua on that machine; the threaded VM beats it on
every workload. Smaller wins add up underneath: immediate-operand
instructions (`fib`'s recursive path went from 10 instructions to 7), a
call path with no function calls of its own, and an assembler with a
table-driven encoder that made lowering about 30% faster.

Every step is measured before it is kept, and the ones that didn't pay are
recorded too, in the [research ledger](docs/implementation/vm-research.md).

### Run them yourself

```sh
cargo bench -p eitri --bench source_to_result     # Eitri (EITRI_THREADED=0 for the match loop)
crates/eitri/benches/lua/run.sh                   # Lua 5.4 (brew install lua@5.4)
```

Both print medians and p99 tails, and write raw samples to
`target/bench-results/`. The workloads live in
[`crates/eitri/benches/fixtures/`](crates/eitri/benches/fixtures/).

## How it works

```text
source ──▶ lex ──▶ parse ──▶ check ──▶ lower ──▶ verify ──▶ execute
           SWAR    Pratt     types &   typed      (debug     tail-call
           scan    parser    names     bytecode   builds)    threaded VM
```

Each stage is a plain function that borrows reusable scratch memory from the
`Context`, so a warm context compiles with only a few dozen allocations.

| Crate | What it does |
|---|---|
| [`eitri-base`](crates/eitri-base) | Typed index vectors, pooled lists, spans, and the context-owned string interner. No dependencies, no global state |
| [`eitri-diag`](crates/eitri-diag) | Diagnostics for every layer: codes, labelled spans, and a renderer in the codespan-reporting style |
| [`eitri-syntax`](crates/eitri-syntax) | The lexer and parser, producing a compact indexed syntax tree |
| [`eitri-check`](crates/eitri-check) | Name resolution, type checking, and host module declarations |
| [`eitri-bytecode`](crates/eitri-bytecode) | The typed bytecode: a machine-readable spec of 163 opcodes, encoder, assembler, verifier, disassembler, and a semantic oracle |
| [`eitri-lower`](crates/eitri-lower) | Lowering of checked code to bytecode: slot allocation, direct compare-and-branch, immediate operands |
| [`eitri-vm`](crates/eitri-vm) | The threaded interpreter, the portable `match`-loop reference, budgets, call depth, and faults with traces |
| [`eitri`](crates/eitri) | The `Context`, the CLI, fixture tests, and benchmarks |

A few house rules keep it honest:

- **No hidden state.** No globals, thread-locals, locks, or reference
  counting (`Rc`/`Arc` are banned by `clippy.toml`). The caller owns the
  context and lends it out.
- **Semantics first.** Performance never changes what a program means.
  Faults, effects, budgets, and evaluation order are the contract.
- **Measure, then keep.** Optimizations need a profile before and a
  controlled comparison after, plus differential tests against a frozen
  reference.

## Development

A dated nightly (`nightly-2026-11-05`, rustc 1.101.0, LLVM 23.1.5) is pinned in
`rust-toolchain.toml` for explicit tail calls (`become`) and the
`rust-preserve-none` calling convention. No external crates are needed.

```sh
cargo build --locked --workspace
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo test --locked -p eitri --test fixtures
cargo test --locked -p eitri --test differential
```

Stress the differential test with more random programs, or rerun one case
and print its source:

```sh
EITRI_DIFF_CASES=100000 cargo test --locked -p eitri --test differential -- --nocapture
EITRI_DIFF_SEED=1234 cargo test --locked -p eitri --test differential -- --nocapture
```

The default suite also runs `limits` (programs large enough for `WIDE`,
`J_X`, and `LDK_X`, and the locals limit), `lifecycle` (a warm context's
heap stays constant over repeated runs; a dropped one leaves nothing), and
`fuzz` (seeded mutations of the corpus through the compiler and of compiled
code through the verifier and both interpreters). Stress the fuzzer with:

```sh
EITRI_FUZZ_CASES=1000000 cargo test --locked --release -p eitri --test fuzz -- --nocapture
```

There is no hosted CI: these commands are the checks, run locally before each
change. Results on other targets are reported only when someone runs them
there (so far: AArch64 macOS on M5 and M2; x86-64 is still owed).

### Fixtures

[`crates/eitri/tests/fixtures.rs`](crates/eitri/tests/fixtures.rs) discovers
every `.eit` file under `crates/eitri/tests/fixtures/` with a matching
`.expect` file and runs it on **both** interpreters and on the program-level
oracle ([`tests/support/oracle.rs`](crates/eitri/tests/support/oracle.rs)), a
tree-walking evaluator over the checked syntax tree written from the
first-slice chapter, sharing only the frontend with the compiler. All three
must agree exactly. Missing or orphan expectations, unknown entries, and an
empty corpus fail the test.

[`crates/eitri/tests/differential.rs`](crates/eitri/tests/differential.rs)
compares the same three on seeded random programs that are well typed by
construction, with literals at the immediate and `i64` boundaries and random
small budgets. Each case is reproducible from its seed.

| Group | Contents |
|---|---|
| `ok/` | Programs that run to completion, with their host effects |
| `fault/` | Overflow, division by zero, budget and depth exhaustion, host faults, with traces |
| `reject/` | Errors inside the supported slice |
| `unsupported/` | Deliberately rejected neighboring features |
| `cascade/` | Half-typed programs, as an editor saves them mid-edit, that must report only their real errors |

A fixture may start with directives:

| Directive | Meaning |
|---|---|
| `//! host: test` | The script may import the harness host module `test` |
| `//! budget: N` | Execution budget in steps (default 1,000,000) |
| `//! depth: N` | Maximum active script frames (default 4,096) |
| `//! pending: TNNN` | The task that makes this fixture pass; until then it must still be **rejected** |

The harness host module `test`:

```eit
pub fn emit(value: i64) -> void     // records the effect
pub fn check(value: bool) -> bool   // records the effect, returns value
pub fn id(value: i64) -> i64        // records the effect, returns value
pub fn fail() -> void               // faults with "requested by fixture"
```

`.expect` holds zero or more `effect: test.f(args)` lines in call order, then
exactly one outcome:

```text
ok
error[E0301] 18..22: type mismatch: expected `i64`, found `bool`
fault: integer overflow
  at bump 69..74
  at <module> 98..129
```

Spans are half-open UTF-8 byte offsets into the whole `.eit` file, directives
included. Expectations are edited by hand and reviewed; tests never rewrite
them.

### Profiling and experiments

| Command | Measures |
|---|---|
| `cargo run --release -p eitri --example path_profile -- vm calls 3` | VM execution only, on one benchmark workload |
| `cargo run --release -p eitri --example path_profile -- stages module` | Compile time per stage (lex, parse, check, lower), raw samples to CSV |
| `cargo run --release -p eitri-check --example frontend_profile` | Frontend throughput in ns/byte |
| `cargo run --release -p eitri-syntax --example lex_profile` | Lexer throughput per byte and token |
| `cargo bench -p eitri-syntax --bench lex_alloc` | Experiment E15: reused scratch against fresh allocation |

Build with `CARGO_PROFILE_RELEASE_DEBUG=2` to profile with symbols.

## Safety

`unsafe` is allowed only with an explained performance benefit: the cost it
removes, why the safe alternative pays it, and the measurement behind it.
Every block carries a `SAFETY` comment, and workspace lints deny
undocumented ones. Today that means two places: the lexer's token buffer
and the threaded VM's frame pointer. In the VM every slot access is still
bounds-checked, so even unverified bytecode cannot cause undefined
behavior; it can only stop the process. Both are checked with Miri:

```sh
cargo miri test -p eitri-syntax -- --skip differential --skip adversarial
cargo miri test -p eitri-vm -- recursion calls_and faults host_calls scratch bool_not
```

---

<div align="center">

*Forged with care, measured with paranoia.* ⚒️

</div>
