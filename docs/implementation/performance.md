# Performance goals

## The target

The common embedding case is a host that starts once and then runs setup
scripts and event handlers many times. Until the compiler and VM exist and are
measured, every number here is a design hypothesis, not a result.

### The reference workload

The reference setup script is a configuration the size and shape of a
respectable Neovim config: [examples/editor-config](../examples/editor-config/).
It has 9 modules and 635 lines (about 20 KB of source), with about 40 option
assignments, 35 keymaps, 9 autocommands, 12 plugin specs with dynamic options,
6 language servers, a statusline function, and a run-time compile of an
optional local file.

| Measure | Target |
|---|---|
| **One module** of about 50 to 100 lines, such as `options.eit` | **Sub-microsecond to a few microseconds**, source text to result |
| **The whole config**, end to end | **Under 100 µs** (proposed), and faster than Lua 5.4 on the equivalent `init.lua` tree |
| **A hot callback**: one call to the statusline's `render()` | Under 1 µs (proposed), with a small number of short-lived allocations that doesn't depend on the buffer's contents; the count is reported per call |

The per-module target is the original "few microseconds" goal. It can't
apply to the whole config: 20 KB of source has to be lexed, parsed, checked,
and compiled, and even at about one nanosecond per byte for all of that, the
source alone costs about 20 µs. The whole-config target is therefore
proposed as throughput-bound, to be confirmed or revised by the first
measurements.

## Workloads

Every workload starts from **fresh source text**. Each is written once in
Eitri and once as an equivalent Lua program, meaning the same observable work.
The reference config is the main workload; the smaller ones isolate one
feature each, so a regression can be traced to its cause.

| Workload | Exercises |
|---|---|
| **Config** | Literals, structs, maps, and enums building a configuration value |
| **Arithmetic** | Integer and float loops |
| **Branching** | `if`, `match`, and pattern-heavy code |
| **Host calls** | Many calls to a resolved host function with scalar and string arguments |
| **Closures** | Creating and calling closures, including captured `var`s |
| **Collections** | Building, indexing, iterating, and looking up arrays and maps |
| **Strings** | Interpolation, concatenation, and string methods |
| **Imports** | A script that imports two small Eitri modules |
| **Errors** | `try`, recovery, and a fault that runs `defer` blocks |

Stress workloads check how compile-time work grows:

| Stress workload | Checks |
|---|---|
| A call chain of 1,000 functions | The module call graph and binding reachability |
| A recursive group of 100 functions | Strongly connected components |
| A module with 1,000 bindings | Bitset width in the initialization check |
| 1,000 functions and 1,000 late bindings, growing together | The quadratic case of binding reachability: time and bitset memory |
| A generic used at 100 type combinations | Instantiation cost and the instantiation cap |

## What is measured

For each workload, separately and end to end:

| Phase | Includes |
|---|---|
| **Compile** | Lexing, parsing, checking, register allocation, and bytecode emission |
| **First execution** | Running the fresh bytecode once, including module initialization |
| **Host boundary** | The cost of each call into and out of the host, apart from the host function's own work |
| **End to end** | Source text in, result out |

Two modes:

- **Single run:** the latency of one compile-and-run, reported as the median
  and 99th percentile over many separate runs.
- **Repeated runs:** 10,000 compile-and-runs, with garbage collection
  included. This catches scratch-memory reuse failing, leaks, and collector
  costs that a single run hides.

Imported Eitri modules initialize once per runtime, so repeated runs in one
runtime would compile and initialize dependencies only the first time. Every
multi-module workload is therefore reported in two cases:

| Case | Setup per run | Measures |
|---|---|---|
| **Fresh modules** | A new runtime from a [template](embedding.md#runtimes), with host modules already registered | Compiling and initializing every Eitri module, as at editor startup |
| **Loaded dependencies** | The same runtime, with imported modules already loaded; only the entry module is fresh | Re-running one module, as when reloading a single file |

Creating a runtime from a template is timed separately and excluded from
both. Host module descriptors are available in both cases, since the host
registers them once.

Reported alongside times: allocation counts and bytes, compiled code size,
and stack-map size.

Measured separately and excluded from the target: reading files, creating a
runtime, and work done inside host functions.

## Method

Published results on interpreter performance have been overturned by compiler
regressions and by changes in hardware (see
[R16–R18](vm-research.md#r16-pessimistic-modern-predictors-erase-most-dispatch-cost)),
so every measurement follows these rules:

- **Pin the host compiler** (Rust and LLVM versions) and inspect the generated
  code of the dispatch loop and hot handlers. A change in the compiler is a
  change in the experiment.
- **Report hardware counters** alongside times: instructions retired, cycles
  per VM instruction, indirect-branch mispredictions per thousand
  instructions, and µop-cache versus legacy-decoder delivery where available.
- **Measure on both vendors and both architectures**: x86-64 from Intel and
  AMD, and AArch64. Store forwarding and µop-cache capacity differ by several
  cycles and several thousand µops between them.
- **Keep negative results.** An optimization that didn't pay is recorded in
  the ledger with its numbers.

## Baseline and environment

- **Lua 5.4** (the reference implementation), loading the same program with
  `luaL_loadbuffer` and running it with `lua_pcall`, in an already-created
  state.
- Both run on the same machine, with frequency scaling disabled. Each report
  records the CPU model, OS, compiler, and build flags.
- Numbers are published only with the workload source and the harness, so
  they can be reproduced.

## Coroutine costs

VM layout and backend comparisons are specified as E1–E9 in the
[research experiment plan](vm-research.md#experiments-and-promotion-gates).
They include checked-tree versus bytecode versus immediate native execution,
encoding/slot widths, dispatch, calls, GC maps, and native publication cost.
These are additional controlled experiments, not new performance results.

These are **benchmark goals**, not commitments, until the VM exists to measure
them. If they are missed, the representation changes, not the semantics.

| Benchmark | Compared against | Goal |
|---|---|---|
| Suspend and resume round trip | Lua `coroutine.yield`/`resume`; a script-to-script call | About 2 ordinary calls |
| `spawn` up to first suspension | Lua `coroutine.create` + `resume` | One stack segment allocation plus a call |
| Joining a finished task | An ordinary call | No suspension, no allocation |
| Per-task memory | A Lua coroutine (about 1 KB) | 256 bytes plus frames |
| Calls in code that never suspends | The same code with no coroutine support | No measurable overhead |

## Design consequences

All of these follow from the founding principle: *resolve what is known,
specialize what is learned, execute directly, and make every recurring cost
earn its place.* See [Overview](../overview.md#the-founding-principle).

The target shapes the language as much as the implementation:

- **Compilation is part of the measured operation.** Work moved to compile
  time is not free. Type inference is local, and the two module-wide checks
  have stated algorithms and cost bounds; see
  [Compiler: module analyses](compiler.md#module-analyses). Nothing looks
  across modules.
- **Short pipeline** with reused scratch memory. See [Compiler](compiler.md).
- **No warmup.** JIT thinking is built into the compiler and VM, so good
  speed never depends on profiling or a warmed-up native tier.
- **Static dispatch everywhere.** Fields are fixed offsets, methods are direct
  calls, and labels and defaults resolve at compile time.
- **No event loop.** Coroutines are stackful and the host drives external
  events. Script-to-script wakeups use one ready queue, which stays empty in
  scripts that never suspend. See [Coroutines and async](../design/coroutines.md).
- **Optimizations are earned.** Quickening and fused instructions are added
  only when measurements justify them. See [Virtual machine](vm.md).
