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
> Eitri is young. The scalar slice (integers, floats, characters, booleans, functions,
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

Every stage is exposed as a subcommand:

| Command | Does |
|---|---|
| `eitri run [--opt] <file.eit\|file.eits>` | Compile (or assemble) and run; `eitri <file>` is the same |
| `eitri check <file.eit\|file.eits>` | Compile (or assemble and verify) without running |
| `eitri asm [--annotate] [--opt] [-o <out>] <file.eit>` | Print the compiled bytecode as `.eits` |
| `eitri verify <file.eits>` | Assemble and verify hand-written bytecode |
| `eitri dump tokens\|ast <file.eit>` | Print the token stream, or the checked syntax tree in the style of `clang -ast-dump` |

A script's host modules come from declaration files (bodiless `pub fn`s):
`<path>.eith` in the script's directory or any directory above it is
registered as the module `path`, and `--host <path>=<file>` names one
explicitly. `run` implements no host functions, so calling one faults; the
other commands only need the declarations. The benchmark workloads and the
fixtures ship theirs (`bench.eith`, `test.eith`).

### Inspecting every stage

Take `calls.eit`, one of the benchmark workloads:

```eit
import bench from "bench"
fn fib(n: i64) -> i64 {
    if n < 2 {
        return n
    }
    return fib(n - 1) + fib(n - 2)
}
bench.emit(fib(15))
```

`eitri dump ast` prints the checked syntax tree in the style of
`clang -ast-dump`: each node's kind, its source range, and its type (in
color on a terminal):

```text
$ eitri dump ast crates/eitri/benches/fixtures/calls.eit
ModuleDecl <line:1:1, line:8:20>
├─ImportDecl <line:1:1, col:25> col:8 bench "bench"
├─FunctionDecl <line:2:1, line:7:1> line:2:4 fib 'i64 (i64)'
│ ├─ParmVarDecl <col:8, col:13> col:8 n 'i64'
│ └─CompoundStmt <col:23, line:7:1>
│   ├─IfStmt <line:3:5, line:5:5>
│   │ ├─BinaryOperator <line:3:8, col:12> 'bool' '<'
│   │ │ ├─DeclRefExpr <col:8> 'i64' ParmVar 'n'
│   │ │ └─IntegerLiteral <col:12> 'i64' 2
│   │ └─CompoundStmt <col:14, line:5:5>
│   │   └─ReturnStmt <line:4:9, col:16>
│   │     └─DeclRefExpr <col:16> 'i64' ParmVar 'n'
│   └─ReturnStmt <line:6:5, col:34>
│     └─BinaryOperator <col:12, col:34> 'i64' '+'
│       ├─CallExpr <col:12, col:21> 'i64'
│       │ ├─DeclRefExpr <col:12, col:14> 'i64 (i64)' Function 'fib'
│       │ └─BinaryOperator <col:16, col:20> 'i64' '-'
│       │   ├─DeclRefExpr <col:16> 'i64' ParmVar 'n'
│       │   └─IntegerLiteral <col:20> 'i64' 1
│       └─CallExpr <col:25, col:34> 'i64'
│         ├─DeclRefExpr <col:25, col:27> 'i64 (i64)' Function 'fib'
│         └─BinaryOperator <col:29, col:33> 'i64' '-'
│           ├─DeclRefExpr <col:29> 'i64' ParmVar 'n'
│           └─IntegerLiteral <col:33> 'i64' 2
└─CallExpr <line:8:1, col:19> 'void'
  ├─MemberExpr <col:1, col:10> 'void (i64)' .emit
  │ └─DeclRefExpr <col:1, col:5> Module 'bench'
  └─CallExpr <col:12, col:18> 'i64'
    ├─DeclRefExpr <col:12, col:14> 'i64 (i64)' Function 'fib'
    └─IntegerLiteral <col:16, col:17> 'i64' 15
```

As in clang, a location prints its line only when it changes (`col:16`
is on the line before it), and a declaration's range runs from its keyword.
A program with type errors still dumps; what the checker could not type
shows `'<error>'`.

`eitri asm` prints the bytecode as `.eits`, the IR: it reads like LLVM IR
over Eitri's actual register machine (`%rN` are frame slots, not SSA
values; a branch falls through when it is not taken):

```text
$ eitri asm crates/eitri/benches/fixtures/calls.eit
; calls.eit

declare void @bench.emit(i64)

define void @<module>() frame(2) {
bb0:
    %r1 = ldi i64 15
    %r0 = call.script i64 @fib(i64 %r1)
    call.host void @bench.emit(i64 %r0)
    ret
}

define i64 @fib(i64 %r1) frame(6) {
bb0:
    cbi.gt %r1, 1, label %bb2
bb1:
    %r0 = mov %r1
    ret
bb2:
    %r2 = subi.chk.s %r1, 1
    %r3 = call.script i64 @fib(i64 %r2)
    %r4 = subi.chk.s %r1, 2
    %r5 = call.script i64 @fib(i64 %r4)
    %r0 = add.chk.s %r3, %r5
    ret
}
```

`n < 2` became one compare-and-branch against an immediate, taken past the
base case (`n > 1`); `n - 1` became one checked subtract-immediate. With
`--annotate`, each line also shows its PC, its encoded words, and the
source range it came from:

```text
$ eitri asm --annotate crates/eitri/benches/fixtures/calls.eit
...
define i64 @fib(i64 %r1) frame(6) {
bb0:
    cbi.gt %r1, 1, label %bb2                   ; 0000  02010152  @57..62
bb1:
    %r0 = mov %r1                               ; 0001  00010000  @80..81
    ret                                         ; 0002  000000a0  @73..79
bb2:
    %r2 = subi.chk.s %r1, 1                     ; 0003  01010226  @103..108
...
```

`.eits` is also an input: `asm` output assembles back to the same words,
and hand-written `.eits` is verified before anything runs (`eitri verify`,
`eitri run`). The format is in
[Bytecode: text form](docs/implementation/bytecode.md#text-form-eits).

**The hot tier.** `--opt` shows what `Context::optimize` does to code that
has run often enough. Here is the loop in `host_calls.eit`
(`while i < 1000 { total += bench.id(i); i += 1 }`) as first compiled:

```text
$ eitri asm crates/eitri/benches/fixtures/host_calls.eit
define void @<module>() frame(4) {
bb0:
    %r0 = ldi i64 0
    %r1 = ldi i64 0
bb1:
    %r2 = ldi i64 1000
    cb.le %r2, %r1, label %bb3
bb2:
    %r2 = mov %r1
    %r3 = call.host i64 @bench.id(i64 %r2)
    %r0 = add.chk.s %r0, %r3
    %r1 = addi.chk.s %r1, 1
    j label %bb1
bb3:
    %r2 = mov %r0
    call.host void @bench.emit(i64 %r2)
    ret
}
```

and re-optimized:

```text
$ eitri asm --opt crates/eitri/benches/fixtures/host_calls.eit
define void @<module>() frame(5) {
bb0:
    %r0 = ldi i64 0
    %r1 = ldi i64 0
    %r4 = ldi i64 1000
bb1:
    cb.le %r4, %r1, label %bb3
bb2:
    %r3 = call.host i64 @bench.id(i64 %r1)
    %r0 = add.chk.s %r0, %r3
    %r1 = addi.chk.s %r1, 1
    j label %bb1
bb3:
    call.host void @bench.emit(i64 %r0)
    ret
}
```

The loop bound `1000` now loads once, before the loop, into a new slot, and
the copies that only fed call arguments are gone: two fewer instructions on
each of the 1,000 iterations. The result is verified again before it
replaces the original, and the test suite runs every program at both tiers
and requires identical effects, faults, and traces. First runs never pay
for any of this.

### Embedding

The host declares what scripts may call, then answers those calls by index.
Arguments and results are read and written through the call context:

```rust
use eitri::{Call, Context, Host, HostError, RunError};

struct Game {
    emit: u32,
}

impl Host for Game {
    fn call(&mut self, cx: &mut Call<'_, '_>, index: u32) -> Result<(), HostError> {
        if index == self.emit {
            println!("{}", cx.arg(0) as i64);
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

`Context::compile_simple` retains T009's comparison baseline: operand
copies, register arithmetic, and materialized comparison results. It uses
the same frontend and execution API. Both lowering modes are checked against
the fixture expectations and independent oracle on both interpreters. See
[Compiler: retained baseline](docs/implementation/compiler.md#retained-scalar-lowering-baseline-t009)
for its limits and IR/optimization API.

A host function can call back into the running script:
`cx.function("on_key")` finds a script function for the current call, and
`cx.call(self, f, &args, &mut results)` runs it on top of that call,
spending the same budget and depth. The handle lives only as long as the
host call that found it, so it can never outlive its program or reach
another program's function; keeping one in the host does not compile. A fault in it comes back
as an `Err`; return it with `?` to stop the outer run with the whole trace.
Because `cx.call` borrows the context mutably, the borrow checker rejects
holding an argument or result view across it.

## Benchmarks

Every number below is **source text in, checked result out**: compile plus
first execution, timed together on every sample. No cached bytecode, no
warmed-up JIT. Each workload is written once in Eitri and once in Lua, and
both harnesses check the answer on every single run, so nothing can be
optimized away.

Medians of 20,001 individually timed runs in an already-created runtime,
same pinned compiler, Lua 5.4.9 built statically with `-O2` and loaded with
`luaL_loadbuffer` + `lua_pcall`.

### Apple M5 (MacBook Pro)

Measured 2026-11-08 on the current tree (host reentry, `.eits`, and the hot
tier included).

| Workload | Eitri | Lua 5.4 | |
|---|---:|---:|---|
| `module`: a 40-line setup script | **5.8 µs** | 9.1 µs | 🟢 1.58× faster |
| `arithmetic`: 1,000 checked-math iterations | **9.9 µs** | 17.1 µs | 🟢 1.73× faster |
| `branching`: `else if` chains, `&&`/`\|\|` | **21.0 µs** | 36.9 µs | 🟢 1.76× faster |
| `calls`: recursive `fib(15)`, 1,973 calls | **12.8 µs** | 16.0 µs | 🟢 1.25× faster |
| `host_calls`: 1,000 calls into Rust | **6.3 µs** | 12.5 µs | 🟢 2.00× faster |

Every host call carries a context that lets the host call back into the
script, which costs about 0.5 ns per call (see the
[ledger](docs/implementation/vm-research.md)); `branching` varies by about
±1 µs between runs.

### Apple M2 (MacBook Air)

Measured at commit `f4573b1`.

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

### Long-running processes

A host that keeps one compiled script and runs it over and over (an editor
hook on every keystroke) pays compilation once. Here each workload is
compiled once and executed 10,000 times; the total includes compiling, and
"per run" is the median of the last 9,000 runs. With **tier-up**, the host
re-optimizes the program once it has run 100 times (`should_optimize`, then
`optimize`), paying for the optimization inside the total. Medians of five
processes, Apple M5, same build as above; Lua loads once and calls the
loaded chunk 10,000 times.

| Workload | Eitri, tier 0 | Eitri, tier-up | Lua 5.4 | Eitri (tier-up) vs Lua |
|---|---:|---:|---:|---|
| `module` | **1.40 ms** (125 ns/run) | 1.49 ms (125 ns/run) | 1.98 ms (167 ns/run) | 🟢 1.33× faster |
| `arithmetic` | 84.4 ms (8.4 µs/run) | **72.8 ms** (7.2 µs/run) | 150.7 ms (15.0 µs/run) | 🟢 2.07× faster |
| `branching` | 171.5 ms (17.0 µs/run) | **159.1 ms** (15.8 µs/run) | 328.8 ms (33.6 µs/run) | 🟢 2.07× faster |
| `calls` | **118.0 ms** (11.7 µs/run) | 118.9 ms (11.8 µs/run) | 148.5 ms (14.6 µs/run) | 🟢 1.25× faster |
| `host_calls` | 55.6 ms (5.5 µs/run) | **48.4 ms** (4.8 µs/run) | 114.1 ms (11.3 µs/run) | 🟢 2.36× faster |

Tier-up pays where there are loops: `arithmetic`, `branching`, and
`host_calls` finish 7–14% sooner, the optimization repaid after 4–8 runs.
It finds nothing to improve in `calls` and the straight-line `module`, so
there it only costs its one optimization (`module` finished 6% later); a
host can skip it when the optimizer reports no changes. First runs never
pay for any of this: tier 0 compiles exactly as before.

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

Both print medians and p99 tails, the hot-tier and long-running tables,
and write raw samples to `target/bench-results/`. The workloads live in
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
| [`eitri-syntax`](crates/eitri-syntax) | The lexer and parser, producing a compact indexed syntax tree; the token dump |
| [`eitri-check`](crates/eitri-check) | Name resolution, type checking, host module declarations, and the clang-style tree dump |
| [`eitri-bytecode`](crates/eitri-bytecode) | The typed bytecode: a machine-readable spec of 163 opcodes, encoder, assembler, verifier, the `.eits` text form and its in-memory IR, the hot-tier optimizer, and a semantic oracle |
| [`eitri-lower`](crates/eitri-lower) | Lowering of checked code to bytecode: slot allocation, direct compare-and-branch, immediate operands; records the IR only when asked |
| [`eitri-vm`](crates/eitri-vm) | The threaded interpreter, the portable `match`-loop reference, budgets, call depth, host reentry, and faults with traces |
| [`eitri`](crates/eitri) | The `Context` (compile, assemble, execute, optimize), the CLI, fixture tests, and benchmarks |

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
cargo test --locked -p eitri --test eits
python3 tools/check_doc_links.py
```

The golden `.eits` listings of the benchmark workloads
(`crates/eitri/tests/eits/`) change only through an explicit, reviewed
update:

```sh
EITRI_BLESS=1 cargo test --locked -p eitri --test eits golden
```

T047's diagnostic snapshots (`crates/eitri/tests/diagnostics/`) cover every
supported code using actual frontend/assembler diagnostics. They record the
structured host data and terminal rendering; source cases reuse the fixture
corpus. Existing renderer tests separately cover ASCII, colors, multiple
files, notes, and layout boundaries. Normal tests never rewrite snapshots.
Update explicitly, then review every changed snapshot:

```sh
EITRI_BLESS=1 cargo test --locked -p eitri --test diagnostics every_supported_code
```

The diagnostic catalog in the first-slice chapter is generated from
`eitri_diag::Code::ALL`. To update or check it without rewriting other text:

```sh
cargo run --locked -p eitri-diag --example diagnostic_catalog -- --write
cargo run --locked -p eitri-diag --example diagnostic_catalog -- --check
```

`python3 tools/check_doc_links.py` checks local Markdown targets and heading
fragments in AGENTS.md, README.md, and the documentation tree. It skips fenced
examples and external URLs; it does not fetch network resources.

Stress the differential test with more random programs, or rerun one case
and print its source:

```sh
EITRI_DIFF_CASES=100000 cargo test --locked -p eitri --test differential -- --nocapture
EITRI_DIFF_SEED=1234 cargo test --locked -p eitri --test differential -- --nocapture
```

The default suite also runs `limits` (programs large enough for `WIDE`,
`J_X`, and `LDK_X`, and the locals limit), `lifecycle` (a warm context's
heap stays constant over repeated runs; a dropped one leaves nothing),
`eits` (every compiling program prints as `.eits` and assembles back to the
same words and behavior), and `fuzz` (seeded mutations of the corpus
through the compiler, of `.eits` text through the assembler, and of compiled
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
pub fn reenter(value: i64) -> i64   // records the effect, runs the script's
                                    // `callback(value)`, returns its result
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
