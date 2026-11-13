# Implementation tasks

Status: T001–T013 and T045–T047 are complete; the M0 gate is closed.
Source now runs end to end: lex, parse, check, lower, verify (debug builds),
and interpret.
Unchecked boxes describe work still to do. This is the executable work breakdown for the
[roadmap](roadmap.md), [execution design](implementation/execution-design.md),
and [research ledger](implementation/vm-research.md).

Work in dependency order. A task is done only when its stated completion
checks pass. Record the change, checks, and any measured result against its
ID. Add scope changes here rather than silently expanding the first runtime.
No timings or implementation dates are promised before a baseline exists.

## Working rules

- Ship a small end-to-end path before implementing all language features.
  Unsupported features get compile diagnostics before any submitted script
  executes. They must never silently acquire different semantics.
- Keep the source language, logical operations, and executable encoding
  distinct. Numeric opcode assignments are internal and generated.
- Preserve source order, faults, budgets, and GC/host contracts when optimizing.
- Run focused semantic tests as features land; use differential and boundary
  tests for the compiler/runtime. Do not make timing thresholds unit tests.
- Compare candidates on the same supported subset and semantics. Report
  allocation, metadata, and code footprint as well as latency.
- Keep experiments behind explicit switches until their promotion gates pass.
  A rejected experiment is a useful result, not an incomplete feature.
- Stage 1–5 in the roadmap are release stages. M0–M6 below are smaller
  implementation milestones within and across those stages.
- Task IDs are stable. New tasks get the next free ID and are placed in the
  milestone where they belong, so IDs are not in reading order.

## Definition of done

Every task, in addition to its own completion checks:

- [ ] Fixtures for the new behavior, including at least one rejected program
  or fault case, in the format from T045.
- [ ] The documentation chapter that specifies the behavior is still true; a
  ⚠️ or "experimental" marker it resolves is removed (T065).
- [ ] Any decision or experiment it touches has its result recorded in the
  [research ledger](implementation/vm-research.md), including rejections.
- [ ] If it changes the compiler or a handler on the source-to-result path,
  the T011 harness is rerun and the result is recorded against the task ID.
- [ ] New `unsafe` code follows the T046 policy.
- [ ] Unsupported neighbors of the feature produce a deliberate diagnostic.

## Task format

Each task lists what it depends on, what it delivers, and when it is done.
Newer tasks also list **Refs**: the documentation that specifies the work
and the decisions (D) and experiments (E) from the ledger it implements. The
[traceability tables](#traceability) map the older tasks the same way.

## Milestones

| Milestone | Outcome | Gate |
|---|---|---|
| M0 | Fresh scalar source executes and calls a typed host function | T001–T012, T045–T047; first reproducible source-to-result measurements |
| M1 | Managed data, results, and closures execute with correct roots | T013–T020; forced-GC and retained-callback checks |
| M2 | Stage 1 core language and embedding workload | T021–T026, T048–T050; explicit Stage 1 corpus and Lua equivalent |
| M3 | Generics, interfaces, extern data, and C embedding | T027–T030, T051; Stage 2 gate |
| M4 | Stackful async, tasks, and channels | T031–T033; Stage 3 gate |
| M5 | Dynamic data, runtime facilities, and the standard library | T034–T036, T052–T056; Stage 4 gate: Lua parity |
| M6 | Measured optimizations and optional native/AOT execution | T037–T043, T057–T060; individual experimental gates |
| Later | Reload, grammar extensions, convenience features, release | T044, T061–T063 |
| Continuous | Security and resource limits; documentation kept true | T064, T065 |

M6 experiments can run as soon as their dependencies exist; they do not
require completing M5. M0 is the immediate critical path. Some M0–M2 tasks
have independent prerequisites, but shared interfaces must be agreed before
concurrent implementation.

## M0: source text to a measured result

### T001 — Bootstrap the Rust library and runner

- [x] Depends on: none.
- Deliver a minimal Cargo package with library and `eitri` binary targets,
  source/test/benchmark-fixture directories, and documented developer commands.
  Use a Cargo workspace with `members = ["crates/*"]`, initially containing
  one `eitri` package with focused modules.
- Done when: a clean checkout builds; formatting and focused tests run;
  the runner reports a structured placeholder error without pretending that
  unsupported source executes. Do not introduce a stable public API yet.
- Completed 2026-11-07: dependency-free Cargo library/binary, pinned Rust
  1.97.1, developer commands in the root README, structured bootstrap `E0001`
  rejection, and source/test/benchmark-fixture directories. Local AArch64
  macOS validation: locked build, formatting, Clippy with warnings denied,
  three CLI tests and four source fixtures pass. No execution timings claimed.
  Workspace follow-up: moved the package to `crates/eitri`; metadata and
  lints are inherited from the root workspace.

### T002 — Freeze the first supported semantic slice

- [x] Depends on: T001.
- Record the supported syntax/types: `i64`, `bool`, locals, assignment,
  checked/wrapping arithmetic, comparisons, `if`, `while`, annotated
  functions, calls, and `return`. Specify top-level success and source traces.
- Decide the initial arithmetic/fuel/resource boundaries, including overflow,
  division edge cases if enabled, temporary initialization, and code ownership.
- Done when: accepted programs and rejected unsupported programs are listed
  as fixtures, with expected results/diagnostics. Forward calls are supported;
  broader inference and syntax are explicitly deferred, not redefined.
- Until T022, function bodies may access parameters, locals, declared function
  symbols, and registered host bindings, but not script module-state bindings.
  Reject that access explicitly so deferring initialization analysis cannot
  admit reads before initialization.
- Completed 2026-11-07: [First supported slice](implementation/first-slice.md)
  records the syntax, types, top-level outcome, traces, arithmetic (including
  the resolved `MIN % -1 == 0`), source-defined budget polls, a frame-count
  call-depth limit, compilation limits, code ownership, and diagnostic codes.
  88 fixtures (16 accepted, 15 faults, 29 rejected, 28 unsupported neighbors)
  carry expected effects, results, diagnostics with spans, and traces. All are
  marked `pending: T008` or `pending: T010` (all live since T010); the harness checks that each is
  still rejected and that its expectation is well formed. Validated locally
  with formatting, Clippy, and the full test suite on AArch64 macOS.

### T003 — Create the machine-readable operation specification

- [x] Depends on: T002.
- Inventory all currently proposed opcode variants (currently 163), but mark
  only the first executable subset implemented. Specify formats, operand
  roles, width/sign/precision, effects, instruction lengths, and fault PCs.
- Generate opcode declarations, the manifest, encoders/decoders, operand
  metadata, disassembly scaffolding, and the 256-opcode capacity assertion.
- Done when: the generated count matches the inventory or an explained
  correction; assignments are deterministic; unsupported operations cannot
  reach execution. Include exact float-complement and `WIDE` legality tables.
- Completed 2026-11-07: `eitri-bytecode` holds the specification as one
  declarative table (`src/spec.rs`, 163 rows): mnemonic, family, format,
  operand roles, structured semantics, effects, and an `implemented` flag
  (30 variants, exactly what lowering the first slice emits). A macro
  derives the `Op` enum in table order (deterministic numbering) with a
  compile-time 256 assertion; encoding and decoding with role range checks,
  automatic `WIDE` for slots above 255, and the two-word forms; lengths;
  `WIDE` legality; condition complements; and a disassembler. Checks: the
  count is 163 and all 18 manifest rows of the bytecode chapter match the
  table (no correction needed); complements are verified by evaluation on
  NaN, infinities, and both zeros at both precisions; every opcode round
  trips at its role boundaries; reserved bits, unknown opcodes, truncation,
  and illegal `WIDE` are rejected. Unimplemented variants reach no
  execution path: none exists yet, and the T005 verifier rejects them.

### T004 — Implement a small independent semantic oracle

- [x] Depends on: T002, T003.
- Implement reference operations for the first subset, with independent
  arithmetic/control-flow logic rather than copying optimized handlers.
  Add narrow-shift/conversion and IEEE comparison models as their variants land.
- Done when: checked/wrapping boundary cases are covered; the float truth
  table includes NaNs, infinities, and signed zeros; narrow integer cases
  can be checked exhaustively at eight bits. No performance claims from this oracle.
- Completed 2026-11-07: `eitri_bytecode::oracle` defines integer
  arithmetic by exact `i128` mathematics range-checked against a width and
  signedness (the handlers use Rust's `checked_*`/`wrapping_*` instead),
  negation, integer conditions from signed/unsigned readings, and float
  conditions from the IEEE partial order (independent of
  `FloatCond::eval`). Checks: i64 checked/wrapping/division boundaries
  (including `MIN % -1 == 0` and `MIN / -1` overflow); every `i8` and `u8`
  pair under all ten operations against Rust's fixed-width arithmetic; the
  float truth table over NaN, infinities, both zeros, and subnormals.

### T005 — Assemble and validate executable functions

- [x] Depends on: T003.
- Build code/constant/site containers, logical instruction IDs, label fixups,
  instruction-start maps, source spans, and final word PCs. Implement `WIDE`
  lengths/legality and far-branch lowering, including stateful loop fallback
  when that operation is enabled.
- Done when: round trips and boundary fixtures cover slots 255/256, signed
  displacement limits, widened extended followers, and illegal branch targets.
  Reject uninitialized slots, incompatible joins, wrong operand types, and
  malformed code in the supported subset, not merely invalid indexes.
- Initially expose only compiler/assembler-produced code to the runtime;
  untrusted serialized bytecode is a separate, deferred API boundary.
- Completed 2026-11-07, except the stateful loop fallback, which applies
  once `LOOP` is enabled. `Function` holds code, a one-slot constant pool,
  call sites, the frame size and signature (results first, then
  parameters), an instruction-start bitmap, PC-to-span and literal-type
  tables. `Assembler` records logical instructions with labels and spans,
  encodes plain instructions on emission, pools constants (`LDI`, `LDK`,
  or `LDK_X` by value), and lays out branches in two passes from
  upper-bound positions (no relaxation loop); a long branch is the
  complement condition over a `J_X`. `verify` checks structure (decoding,
  implemented opcodes, slots in frame, constants, sites and call windows
  against signatures, disjoint host windows, targets on instruction
  starts, no fall-through, exact start map) and types by a dataflow to a
  fixpoint (uninitialized and conflicting slots, operand types, `bool`
  literals). Checks: 11 tests, including branches at the exact short/long
  boundaries both directions, slots 255/256, widened `LDK_X` followers,
  65,537 pooled constants, branches into operand words and `WIDE`
  followers, and joins that conflict only where read.

### T006 — Implement the scalar register interpreter

- [x] Depends on: T004, T005.
- Use portable Rust dispatch, untagged eight-byte slots on 64-bit targets,
  explicit control frames, and non-overlapping script call frames. Implement
  only the enabled operations and checked argument/result transfers.
- Done when: hand-assembled programs agree with the oracle; script recursion
  uses bounded VM storage rather than Rust handler recursion; calls and taken
  backward edges share the host-call budget; faults carry the correct PC.
- Completed 2026-11-07: `eitri-vm` interprets verified functions with a
  portable `match` (the D9 baseline): untagged slots on one growable stack,
  control frames separate, script arguments copied into the callee frame
  and results copied back, host windows borrowed in place (the verifier
  guarantees they are disjoint). Every call and taken backward branch
  debits the budget first; depth counts script frames below the entry.
  Safe Rust throughout. Checks: 8 tests: every implemented arithmetic,
  negation, compare, and compare-and-branch opcode agrees with the oracle
  on edge and seeded values, faults included; 100,000-deep recursion runs
  without Rust recursion and the depth limit faults with the full trace;
  exact shared budget accounting across calls and backward jumps; fault
  PCs through calls and `WIDE`; host faults; scratch reused without growth.

### T007 — Implement source storage, interning, and the core lexer

- [x] Depends on: T001, T002.
- Build source/span ownership, dense spelling IDs with collision-safe lookup,
  compact token buffers, delimiter indexes, and resettable compilation scratch.
  Apply the specified newline and operator rules for the supported syntax.
- Done when: identifiers are not mistaken for bindings; shadowed spellings
  share spelling IDs; malformed input has correct spans; repeated compilation
  resets scratch without invalidating published source metadata.
- Completed 2026-11-07. `eitri-base` provides the base types:
  `newtype_index!` ids (`NonZeroU32`, so `Option<Id>` is four bytes; no
  `unsafe`), `IndexVec`, `IdRange`, `List`/`ListPool`, `Span`/`SourceId`,
  the Fx hasher, and a context-owned `Interner` (no lock or leaked memory;
  one byte buffer; open addressing comparing a hash tag before bytes;
  keywords and predeclared names are predefined constant symbols; `reset`
  removes only the symbols added since the last reset). `eitri-syntax` adds
  the lexer: the whole lexical grammar (so later stages can reject
  unsupported features precisely), parallel-array tokens of 13 bytes with
  the symbol or matching-delimiter index as payload, the specified newline,
  `>`, and `try?` rules, string and interpolation scanning with an explicit
  stack, number spellings validated but not converted, delimiter matching
  with unclosed/unexpected reports, and the 256-level nesting limit
  (`E0002`). `Context` keeps the interner and a `Scratch` struct with one
  field per kind of reusable working memory (D44); `lex` borrows its
  `LexScratch`, clears it on entry, and returns the tokens it filled.
  Lexical errors are returned, all of them.
  Checks: 21 lexer tests (token shapes, newline rules, delimiters, literal
  and error spans, linear-time adversarial inputs, no memory growth across
  reused scratch across different-sized runs) and 13 base tests; fixtures `reject/malformed_literal` and
  `reject/unterminated_comment` are now live. Decided: a line break inside a
  block comment counts (recorded in Lexical structure). Known limit: a line
  break inside an interpolation of a single-line string ends the literal and
  the next line lexes separately (follow-on errors; T047 recovery may
  improve it). E15 measured the lexer at about 2 ns per source byte (2.2 µs
  for a 1 KB module) and per-run allocation at 2–4% of that on real
  modules; see the research ledger. E2 first pass: lexing went from 1.88
  to about 1.30 ns per byte (−31%) through measured steps, with rejected
  attempts recorded; a frozen reference lexer and a 20,000-case
  differential test guard every optimization.

### T008 — Parse, resolve, and check the scalar subset

- [x] Depends on: T007.
- Build compact indexed syntax, lexical scopes and binding IDs, preliminary
  function signatures, expected-type checking, assignment rules, return-path
  checks, and scalar control-flow typing. Require annotations in this slice.
- Done when: forward calls, shadowing, precedence, and source-order fixtures
  pass. Any error anywhere in the submitted slice prevents all its setup
  effects; unsupported syntax receives a deliberate diagnostic.
- Frontend complete 2026-11-07; the done check finishes when T010 executes
  the `ok/` fixtures. `eitri-syntax` parses into a compact tree (nodes in
  index vectors, 16-byte expressions, pooled child lists, `if` in a side
  table), rejects every unsupported neighbor with one `E0001` and drops it
  whole, resynchronizes after syntax errors, and limits nesting to 256
  (`E0002`). `eitri-check` collects all signatures first (forward calls),
  resolves names through per-spelling heads with an undo log (O(1) lookup),
  assigns binding ids (shadowing), checks types against expectations,
  assignability, call arity, and return paths, and walks long operator
  chains iteratively (100,000 operators checked without recursion). Host
  declaration modules are registered by signature (`Context::
  register_host_module`), with their names committed to the interner.
  Decided: function results come only from `return` (first-slice chapter).
  Checks: 55 diagnostic fixtures now live (all of `reject/` and
  `unsupported/`), 22 new `cascade/` fixtures, and the harness requires the
  31 execution-pending fixtures to pass the whole frontend cleanly; 36
  syntax tests, 7 checker tests. E2 second pass: parsing 1.36 → about
  1.04 ns/byte and checking 0.89 → about 0.69 ns/byte (whole frontend 3.87 →
  about 3.35 ns/byte) under differential tests against frozen copies of
  this parser and checker; it also fixed unbounded recursion on long `??`
  chains. Reference counting (`Rc`, `Arc`, `Weak`) is banned by `clippy.toml`.
- Complete 2026-11-07: with T010, all 16 `ok/` fixtures execute and pass.

### T009 — Lower checked bodies into typed bytecode

- [x] Depends on: T005, T008.
- Connect checked binding/type IDs to slots, constants, call sites, branches,
  and source information. Start with cheap temporary reuse and destination
  passing; retain a simple lowering mode for later optimization comparisons.
- Done when: source programs and their oracle equivalents agree, no dynamic
  name/type lookup remains in ordinary scalar handlers, and the disassembler
  exposes the actual emitted words and frame size.
- Implemented 2026-11-07 in `crates/eitri-lower`: one function per script
  `fn` plus `<module>` (function 0). Bindings get fixed slots, temporaries
  a stack-ordered counter; names are read in place (D37); results are
  written to their destination (D38). Conditions compile to one
  compare-and-branch on the complement; `&&`/`||`, operator, and `else if`
  chains lower iteratively (100,000-operator chains tested). `while` is
  top-tested with the budget-polling back edge spanned at `while`.
  Unreachable statements after `return` are not emitted. Every fixture
  lowers to code the verifier accepts; 5 unit tests. Open: a retained
  simple lowering mode.
- Program-level oracle 2026-11-07: `crates/eitri/tests/support/oracle.rs`
  evaluates the checked syntax tree directly (no slots or bytecode), with
  arithmetic and comparisons from the T004 oracle and budget/depth rules
  from the first-slice chapter. Every fixture that compiles (33 of the 114
  in the corpus) and every differential case (T012) must produce the same
  effects, result, and fault trace on it as on both interpreters. Mutation
  checks: polling a loop before its body, debiting a call before its
  arguments, and `>=` as `>` are each detected. The last was missed by
  the corpus alone; `ok/comparison_boundaries` now covers equal, adjacent,
  extreme, and immediate-range operands for every comparison.

- Completed 2026-11-12 (T009/T010/T012 step 1): the retained simple
  lowering baseline is `eitri_lower::lower_simple` / `Context::compile_simple`.
  It copies binding operands, uses register arithmetic and materialized
  comparisons, and retains destination passing and temporary reuse. Const
  specialization keeps mode tests off default lowering. Both modes verify
  and agree with both interpreters and the independent oracle on all 124
  fixtures (0 pending), 100,000 generated programs, and encoding/budget/depth
  limits. Two new fixtures cover destination reads and comparison-fault
  effect order; iterative lowering is checked in both modes. Existing
  annotated disassembly and frame reporting remain available, and benchmark
  golden words are unchanged. Seven lowering tests pass. Full workspace
  build, format, Clippy (`--all-targets -- -D warnings`), and tests pass.
  T011 was rerun before/after, on both interpreters after, and against Lua;
  see the [step 1 ledger](implementation/vm-research.md#t009t010t012-retained-simple-lowering-and-semantic-gate--2026-11-12)
  for exact commands, raw samples, latency and memory results. No new unsafe.

### T010 — Connect typed host calls and the runner

- [x] Depends on: T006, T009.
- Register one scalar host signature by index, execute submitted source, and
  return structured compile errors, success, or faults. Capture observable
  host effects in fixtures. Keep the first call context thread-bound.
- Done when: a script computes, branches, calls another script function, and
  passes a checked scalar result to the host. A later compile error causes
  zero host effects. Nested host reentry shares the existing budget.
- Implemented 2026-11-07: `Context::run(source, &mut dyn Host)` returns
  `RunError::Compile(diagnostics)` or `RunError::Fault(FaultReport)`; host
  functions are called by `Context::host_index`; `set_budget`/`set_depth`;
  lowering and VM scratch live in the context. Faults become diagnostics
  with the trace as labels (identical recursive frames collapse into one).
  The CLI exits `3` on a fault. All 31 execution-pending fixtures are live
  (110 fixtures pass, none pending).
- Nested host reentry 2026-11-07: `Host::call` receives a `Call` context;
  arguments and results are read and written through it (still in place
  in the caller's frame), and `cx.call(self, f, args, results)` runs a
  script function of the same program on the same slot and frame stacks,
  debiting the shared budget and counting toward the depth limit; at most
  64 nested runs (first-slice chapter, "Host reentry"). A nested fault is
  returned to the host, which may propagate it (`HostError::Fault`) with
  the trace continued through the host function (`Location::Host`). A
  `compile_fail` doctest shows an argument view cannot be held across
  reentry. Both interpreters, the oracle, the harness `test.reenter`, 8
  fixtures, the nesting limit (`limits.rs`), and the differential generator
  cover it; 100,000 differential cases agree. Cost: about 0.5 ns per host
  call on the threaded interpreter
  ([ledger](implementation/vm-research.md#host-reentry-the-call-context--2026-11-07)).
  Every "done when" check now passes; T010 stays open only on its
  dependency T009 (the retained simple lowering mode).
- Audit fixes 2026-11-08: a cached `ScriptFn` used with another program
  called that program's function at the same position. A first fix tied it
  to its program's function-table address plus a name hash; a second audit
  showed that identity reused after the program was dropped (99 of 100
  trials). `ScriptFn<'call>` is now valid only during the host call that
  found it (`Host::call` gets a fresh lifetime each time), so a host
  cannot keep one; a `compile_fail` doctest shows it, and
  `tests/reentry.rs` checks each program reenters its own `callback`.
  Hosts look a function up by name on each call; retained callbacks come
  with the embedding API (T024). `Context::optimize` compared only words,
  constants, and sites, so `return 1` was accepted as the source of
  `return true`; it now compares whole functions (signatures, literal
  types, frames, spans). Literal hoisting searched its hoisted literals and
  preheader entries linearly (quadratic: 1.8 ms at 1,000 statements, 38.9
  ms at 8,000); a map and one merge make it linear (0.23 ms and 1.86 ms).
  The verifier kept a frame state per instruction: 1,000 instructions
  naming `%r65535` peaked at 65.9 MB; it keeps one per basic block now,
  396 KB (`tests/verify_memory.rs`). Memory is O(blocks × frame); the same
  300,000 mutated programs verify as before.

- Closed dependency gate 2026-11-12 (step 1): T009's simple mode now
  exists. `cargo test --locked --workspace` passes, including host reentry
  and its lifetime doctests; `cargo test --locked -p eitri --test fixtures
  --test limits --test lifecycle --test reentry --test eits -- --nocapture`
  passes (124 fixtures, 5 limit tests, 3 reentry tests, 2 `.eits` tests, and
  lifecycle). The fixture comparisons include the simple mode on both VMs,
  with identical ordered effects, fault spans/traces, and shared budgets.
  This closes T010; it does not implement T024's retained handles.

### T011 — Establish the first performance harness

- [x] Depends on: T010.
- Add arithmetic, branch, script-call, and minimal-host-call fixtures and
  equivalent Lua 5.4 sources. Measure compilation, first execution, total
  latency, allocations, scratch high-water, frames, code, and metadata bytes.
- Done when: release results include machine/build details and raw samples;
  a reused initialized runtime is distinguished from fresh runtime creation;
  compilation and first execution are included in every source-to-result claim.
  Keep an actual observable result so benchmark work cannot disappear.
- Complete 2026-11-07: `cargo bench -p eitri --bench source_to_result`
  and the Lua 5.4 baseline `crates/eitri/benches/lua/run.sh` measure five
  workloads (module, arithmetic, branching, calls, host calls) with compile
  and first execution timed separately in every sample, reused and fresh
  runtimes distinguished (creation reported separately), p50/p99 over
  20,001 samples, raw CSV samples, allocations and bytes per run, context
  scratch retained after warm-up, code and metadata bytes, and frame
  slots. Every run's result is checked. `Context::compile` and
  `Context::execute` split `run` so the phases can be timed. Results are in
  the [research ledger](implementation/vm-research.md#t011-baseline-source-to-result-against-lua-54--2026-11-07):
  compilation beats Lua 5.4 by 1.2–1.4×, execution trails by 1.25–1.9×,
  and the 1.2 KB module takes 6.6 µs, above the few-microsecond target.

- Profiling follow-up 2026-11-07: `path_profile` isolates VM execution,
  full compilation, lowering, and individual compile stages on T011 fixtures.
  [Recorded profiles](implementation/vm-research.md#e4e5-and-t011-path-profiles--2026-11-07)
  identify shared VM decode/dispatch work, script argument/result copies,
  instruction encoding and assembler finalization. Full correctness and
  Eitri/Lua performance harnesses rerun; no optimization promoted and
  T037/T039/T058 remain incomplete.

### T012 — Close the first end-to-end semantic gate

- [x] Depends on: T010, T011.
- Differentially exercise supported source/control-flow combinations and
  encoding limits. Add malformed-input/bytecode fuzz entry points, execution
  limits, and repeated compile/run/dispose checks.
- Done when: unsupported code cannot execute, valid boundary cases agree
  with the oracle, loops/recursion return control on budget exhaustion, and
  repeated runs show no unbounded retained artifact growth. Record the
  baseline even if it misses the latency target.
- In progress 2026-11-07: `crates/eitri/tests/differential.rs` generates
  seeded programs of the first slice, well typed by construction (up to
  four functions with acyclic calls, `let`/`var`, every assignment
  operator, `if` statements and values with `else if`, counted `while`
  loops, every operator, host calls, literals at the 8-bit, 16-bit, and
  `i64` boundaries, and a small random budget in 30% of cases), and
  requires both interpreters and the program-level oracle to agree. 300
  cases by default; 100,000 cases agree locally (debug build, so the
  verifier also checks every lowered program): 49,164 completed, 39,382
  overflow, 7,675 division-by-zero, and 3,779 budget faults, with 465,485
  host effects compared. Not generated: call depth faults (calls are
  acyclic; fixtures cover them).
- Encoding limits 2026-11-07: `crates/eitri/tests/limits.rs` runs, on both
  interpreters and the oracle, 300-local functions (`WIDE` operands), far
  forward branches from comparisons (past 127 words), `bool` tests (past
  32,767 words), and a loop exit (each a complement over `J_X`), and
  70,000 distinct constants (`LDK_X`); a lowering test confirms the same
  shapes emit those forms. 65,535 module locals run and 65,536 are
  `E0002`, as the chapter specifies. Loops test at the top, so no
  backward conditional branch exists to widen; a back edge is a 24-bit
  `J`. The `E0002` diagnostic for `<module>` has an empty span (0..0).
- Lifecycle 2026-11-07: `crates/eitri/tests/lifecycle.rs` counts live heap
  bytes with an allocator installed in that test binary only. Over the
  119-source corpus (fixtures and benchmark workloads, all outcomes), a
  warm context retains exactly the same bytes across 20 further passes
  (423,525 B with the `match` loop, 554,597 B threaded, at 10,000-step
  budgets), and creating, using, and dropping a context returns live
  bytes to the baseline exactly, under both interpreters.
- Fuzz entry points 2026-11-07: `crates/eitri/tests/support/fuzz.rs` has
  `source(bytes)` (never panics; rejections report an error and no `bug:`,
  with valid ranges; accepted programs agree across both interpreters and
  the oracle) and `bytecode(functions, hosts)` (verifying never panics;
  verified code runs on both interpreters without panicking and with equal
  outcomes). `crates/eitri/tests/fuzz.rs` drives them with seeded
  mutations of the corpus, without external crates. The first large run
  found two verifier gaps, now fixed with regression tests in
  `eitri-bytecode`: a call whose opcode did not match its site's target
  kind verified, and reached `unreachable!` in the threaded handlers (a
  panic, not undefined behavior); and a frame smaller than its signature
  panicked the verifier. After the fix, 1,000,000 cases per target pass in
  debug and in release (80,738 mutated sources compiled and agreed; 56,074
  mutated programs verified and ran). Not yet run under Miri or a
  coverage-guided fuzzer.
- Every "done when" check above now has recorded evidence (the baseline
  is the T011 record), and reentry cases are now in the fixtures,
  differential, and fuzz runs. T012 stays open until its dependency T010
  closes, which waits only on T009.

- Completed 2026-11-12 (step 1): T009 and T010 are closed. The four
  workspace checks pass. `EITRI_DIFF_CASES=100000 cargo test --locked -p
  eitri --test differential -- --nocapture` reports 100,000 agreeing cases
  and 887,041 compared host effects across both lowerings, both VMs, the
  hot tier, and the independent oracle. `EITRI_FUZZ_CASES=1000000 cargo test
  --locked -p eitri --test fuzz -- --nocapture` and its `--release` variant
  each pass one million inputs per source, assembly, and bytecode target:
  100,440 sources compile and agree, 28,171 assembly inputs assemble and
  agree, and 51,699 bytecode inputs verify and run on both VMs. Limits and
  rejection fixtures pass; no unsupported script effects occur. Over 129
  corpus sources, lifecycle checks retain exactly stable warm bytes across
  20 further passes: default match/threaded 423,706/554,778 B; simple
  match/threaded 423,686/554,758 B. Dropped contexts return to baseline.
  T011 records the remaining latency-target gap; no optimization promoted.
  These are seeded mutation runs, not coverage-guided fuzzing or Miri.

### T045 — Build the test and fixture infrastructure

- [x] Depends on: T001.
- Refs: [Performance: method](implementation/performance.md#method).
- Define one fixture format: source files, the host bindings it may use,
  and the expected outcome (result value, structured diagnostics, fault with
  trace, recorded host effects, or budget exhaustion). Add golden
  disassembly snapshots, an oracle hook (T004), and a runner that executes
  every fixture under the interpreter and, later, every tier.
- Done when: a fixture can assert each outcome kind; a failing fixture
  prints a readable diff; fixtures run in parallel and deterministically;
  updating golden output is an explicit command, never automatic.
- In progress 2026-11-07: Rust integration harness discovers paired `.eit` /
  `.expect` files, validates the corpus, runs isolated cases in bounded
  parallel batches, and reports expected/actual diagnostics and byte spans.
  Snapshots are edited explicitly. Updated with T002: recursive discovery,
  `host`/`budget`/`depth`/`pending` directives, the harness `test` host
  module, and an expectation format for effects, `ok`, diagnostics, and
  faults with traces, validated even while pending. Execution adapters
  landed with T010; the program-level oracle adapter (T009) runs every
  compiling fixture.
- Complete 2026-11-07: disassembly snapshots are golden `.eits` listings
  of the five benchmark workloads (`crates/eitri/tests/eits/`), updated
  only by `EITRI_BLESS=1 cargo test -p eitri --test eits golden`. `.eits`
  ([Bytecode: text form](implementation/bytecode.md#text-form-eits)) is
  printed and parsed by `eitri_bytecode::text`; every compiling fixture,
  benchmark workload, encoding-limit program, and differential case prints
  and assembles back to the same text, the same words (annotated listing),
  and the same behavior on both interpreters (30,000 differential cases
  locally). A third fuzz target mutates `.eits` text (300,000 cases each in
  debug and release); its first run found a slot past `%r65535` that
  overflowed the assembler's frame count in debug builds, now rejected
  (`E0901`) and saturated in the assembler.

### T046 — Pin the toolchain and set the `unsafe` policy

- [x] Depends on: T001.
- Refs: D24; [Performance: method](implementation/performance.md#method).
- Pin the Rust toolchain (D24), and record it with every benchmark.
- `unsafe` policy: permitted with an explained performance benefit (cost
  removed, safe alternative, measurements or planned experiment). Each block
  states its invariants and why they hold in a `SAFETY` comment; frame,
  slot, and handler code runs under Miri where Miri supports it; the C API
  (T030) runs under AddressSanitizer and UndefinedBehaviorSanitizer.
- Done when: the toolchain is pinned, lints enforce the `SAFETY` rules, and a
  deliberately broken `unsafe` invariant is caught by the configured tools.
- Scope change 2026-11-07: hosted CI and its target matrix were removed by
  the project owner. Checks are the documented local commands; results
  from other targets (x86-64, Windows) are reported only when someone runs
  them there, and never inferred.
- Scope change 2026-11-07: the pin moved from Rust 1.97.1 to
  `nightly-2026-11-05` at the project owner's request, for `become` and
  the `rust-preserve-none` convention (E5). Still a dated pin: a compiler
  change is a change in every experiment. Miri now ships with the pinned
  toolchain.
- Completed 2026-11-07: toolchain pinned (`rust-toolchain.toml`); workspace
  lints deny undocumented unsafe blocks and implicit unsafe operations
  inside unsafe functions; the policy is in the root README. The first
  `unsafe` block (the lexer's token push, E2 step 5) passes Miri locally,
  and a deliberately overstated capacity is reported by Miri as an
  out-of-bounds access. Sanitizer coverage arrives with the C API (T030).

### T047 — Implement structured diagnostics and error recovery

- [x] Depends on: T008.
- Refs: [Compiler: diagnostics](implementation/compiler.md#diagnostics).
- Give each diagnostic a stable code (`E0204`), severity, message, and
  source ranges; render the documented terminal form and hand the host the
  same data. Recover at the next statement or declaration boundary, up to a
  cap, so one run reports several errors. Valid scripts pay nothing for
  recovery.
- Done when: snapshot tests cover every code in the supported slice; one
  file with several independent errors reports each once; recovery never
  produces a cascade of follow-on errors for one mistake; the code catalog
  is a generated list, kept with T065.
- In progress 2026-11-07: the `eitri-diag` crate provides the shared model
  (severities warning, error, fault, and bug; `E`-codes; primary/secondary
  labels over `u32` byte spans with source ids; notes and help), a capped
  `Diagnostics` sink for recovery, and a renderer matching the features of
  codespan-reporting 0.12 that Eitri needs: rounded box drawing with an
  ASCII fallback, rich/medium/short styles, configurable ANSI styles,
  multi-line labels as nested brackets, start/end context for long spans,
  context lines around labels, stacked messages, multi-file sections, tab
  expansion, approximate wide/zero-width columns, and clamped invalid spans.
  Not adopted: a `Files` trait (the context owns sources) and a termcolor
  dependency. The CLI renders through it. 21 rendering tests plus
  width-table checks pass locally. Update with T008: recovery in the
  lexer, parser, and checker, with the cascade rules in the first-slice
  chapter and 22 `cascade/` fixtures of half-typed programs. Remaining:
  snapshot coverage of every code and the generated catalog.

- Completed 2026-11-12 (step 2, with T065): snapshots of real compiler and
  assembler diagnostics cover all 17 supported codes, including severity,
  messages, source labels/ranges, and default terminal output. Source cases
  reuse T045 fixtures; E0204 stays reserved for T022. The snapshot test
  rejects drift and orphan files; updates require explicit `EITRI_BLESS=1`.
  Three new combined-error fixtures exercise lexical, parser, and checker
  recovery in source order with no duplicate cascades or prior host effects.
  Boundary tests store at most 100 reports for 1/99/100/101/300 independent
  mistakes per stage and successfully reuse the context afterwards.
- Checks: `cargo build --locked --workspace`, `cargo fmt --all --check`,
  `cargo clippy --locked --workspace --all-targets -- -D warnings`, and
  `cargo test --locked --workspace` pass. Focused renderer/catalog tests
  and `cargo test --locked -p eitri --test diagnostics --test fixtures
  --test lifecycle -- --nocapture` pass: 17 code snapshots, 127 fixtures
  (0 pending), and stable lifecycle storage. The 21 existing renderer
  snapshots still cover display variants and layout boundaries.
- The first-slice catalog is generated from `Code::ALL` with
  `cargo run --locked -p eitri-diag --example diagnostic_catalog -- --write`;
  its `--check` mode and ordinary tests detect drift without writing.
  Generator tests enforce one marker pair and preserve surrounding text.
  Deliberately stale catalog/snapshot controls fail as expected, then are
  restored. `python3 tools/check_doc_links.py` passes for local Markdown
  targets/fragments; one stale README fixture anchor was corrected. See the
  [T047/T065 ledger](implementation/vm-research.md#t047t065-diagnostic-snapshots-generated-catalog-and-m0-documentation--2026-11-12).
  No compiler/runtime/renderer implementation, supported syntax, diagnostic
  code, or message changed. T011 was not rerun because the source-to-result
  path is unchanged; the T009/T010/T012 baseline remains the latest result.
  No new unsafe or dependency; no open specification question was needed.

**M0 exit:** a usable measured scalar slice. Completing all 163 operations,
the collector, the editor config, or a native backend is not an M0 dependency.

## M1: managed values and callbacks

### T013 — Complete scalar representations and conversions

- [x] Depends on: T012.
- Add `i8`, `u8`, `u64`, `f32`, `f64`, `char`, conversions, bitwise operations,
  width-aware shifts, and precise float arithmetic/comparisons. Keep storage-only
  extern widths distinct from ordinary source types.
- Done when: canonical slot representations and conversion faults agree
  with reference models, including narrow arithmetic and all float conditions.

- Completed 2026-11-12: added canonical `i8`/`u8`/`u64`/`f32`/`f64`/
  `char` slots, contextual literals, checked and integer-only truncating
  conversions, bitwise operations, width-aware shifts, IEEE arithmetic and
  all float conditions. Storage-only extern widths remain unavailable as
  ordinary source types. Both interpreters, both lowerers, the hot tier, and
  text bytecode round trips cover the new scalars; independent integer and
  IEEE-field reference models cover narrow arithmetic and conversion faults.
  Completion audit fixed count-driven narrowing of a shift's left literal;
  added all-count-type, decimal rounding, signed underflow, negative infinity,
  arithmetic boundary, and unsupported-neighbor regressions.
- Completion checks: locked workspace build, format check, Clippy with
  warnings denied, and workspace tests (185 pass) succeed; 151 fixtures,
  zero pending. Diagnostic catalog freshness and local documentation links
  pass. Local Apple M5 / AArch64 macOS only; no x86-64 or sanitizer claims.
- T011 rerun: 20,001 samples per cell, default threaded release build;
  reused-context module compile/execute/total p50 = 7.500/0.208/7.708 µs,
  total p99 = 15.4 µs. The few-microsecond module goal remains missed.
  Memory, environment, method, raw samples, and limitations are recorded in
  the [T013 ledger](implementation/vm-research.md#t013-scalar-completion-audit--2026-11-12).
  Review summary and check output: `/tmp/codex-eitri-T013.md`.

### T014 — Define layouts and root descriptors

- [ ] Depends on: T013.
- Implement canonical types, aggregate layouts, ordinary reference maps,
  dynamic-root recipe support, helper roots, and pre-call/suspended/post-call
  root states. Distinguish tracing safepoint PCs from precise fault PCs.
- Done when: layout tests cover padding, inactive union slots, nested
  references, and reused temporaries; every allocation-capable path has a
  defined root publication point before allocation.

- Step 1 landed: caller-owned `eitri-types` canonical descriptors and owned
  portable signatures compare structure across contexts without comparing local
  IDs. Host-binding snapshots/cache comparisons use these encodings; successful
  execution allocates nothing for signature validation. Compound constructors
  remain internal to the type crate's tests; no language/registration support
  was added. The owner resolved named identity as stable (module path,
  declaration name) plus recursively equal shape; steps 2–3's nominal parts
  are unblocked but not implemented. The dynamic type-test sentence is corrected:
  an i64 converted to dyn matches i64. T014 remains incomplete.

- Step 2 landed: checked Frame/Heap/ExternC layout arithmetic in `eitri-types`,
  with test-only anonymous products, compact fixed-array Repeat trace descriptors,
  and distinct managed-reference/raw-pointer/host-identity leaves. An explicit
  work stack rejects inline cycles; reference edges terminate inline traversal.
  Unit tests cover padding, nested offsets, scalar roots, array stride/zero counts,
  overflow, deep graphs and target Rust `repr(C)` agreement; 512 deterministic
  generated shapes agree with an independent byte-offset calculator. No language,
  registration, nominal type or VM changes. Nominal parts are now unblocked.
  Anonymous split-union/dynamic work is recorded in step 3 below; helper roots,
  call-state maps, verifier obligations and runtime publication remain later work.
  Collection and aggregate execution remain T015/T017 work; T014 is not complete.

- Step 3 (anonymous tagged values) landed: Optional/Result split-union layouts
  have explicit tags, separate reference/scalar partitions, per-case transfers
  and null-initialization plans. Compact CopyPlan/Repeat data composes through
  products and arrays without unrolling elements. Synthetic dynamic-root recipes
  compose through products/arrays and now through active-case union guards.
  Tests cover every anonymous case, nested optional/result ordering, transitions, cleared inactive references,
  full i64 payload bits and an independent semantic label visitor. No nominal
  enum, dyn language behavior, registration or VM support was added. The owner
  resolved Q1/Q2 in the reference chapters. The dynamic representation gate is
  active-case guards: tag first when tracing/copying, inactive pairs never read,
  and tag last after payload initialization when publishing (no safepoint inside
  in-place mutation). Nested dynamic copies and poisoned inactive-pair tests pass.
  Helper-root scopes and steps 4–7 runtime publication/verifier work remain.
  T014 is still incomplete; real GC and aggregate execution remain T015/T017 work.

- Step 3 case tables generalized: anonymous unions have dense u32 tags and at
  most u32::MAX cases, with checked bounds, case-specific initialization,
  copying and guarded dynamic traces. Optional/Result remain two-case instances.
  Tests cover three distinct root shapes, poisoned inactive pairs and a
  1,000-case descriptor. Named types and enum declarations remain for 9b/9c.

- Named-product declaration graph step 9b1: two-phase declarations and iterative
  inline-cycle validation, plus synthetic named-product layout, are implemented.
  Cross-store shape equality uses iterative coinductive pair comparison (9b2);
  portable recursive signatures use bounded, iterative path back-references (9b3);
  Named enum graph and split-union layouts landed (9c1–9c2); language and registration remain later work.
- Step 4 (10a–10b): root metadata, structural and scalar coverage checks landed; synthetic managed facts covered; checks and measurements: `/tmp/orchestra/eitri-10b-done.md`.
- Step 5 (11–11b, partial): bounded builder, verified diagnostic boundary, root-safe optimizer fallback and scalar type/root accounting landed; synthetic managed CFG lowering and layout accounting remain.

### T015 — Implement allocation and non-moving collection

- [ ] Depends on: T014.
- Add memory accounting, non-moving mark-sweep, runtime/host roots, artifact
  roots, retained handles, and generation-checked host identities. Implement
  precise roots first; conservative scanning remains an independent experiment.
- Done when: forced collection at every legal safepoint preserves all live
  data and reclaims unreachable graphs; handle release and allocation failure
  are tested; memory limits include retained compiler/code metadata.

### T016 — Implement strings, bytes, and characters

- [ ] Depends on: T013, T015.
- Add immutable UTF-8 strings, binary bytes, literals, content equality/hashes,
  runtime literal interning, byte/character access, and essential conversions.
- Done when: separately compiled equal strings hash equally, UTF-8 boundaries
  are respected, source lifetime is safe, and no runtime interning assumption
  replaces required content comparison.

### T017 — Implement aggregates and tagged values

- [ ] Depends on: T014, T015, T016.
- Add structs/classes, fixed arrays, enums and indirect payloads, optionals
  with collapse, results, defaults, construction, and core matching.
- Done when: copies and shared identity follow the language; large aggregates
  use bounded code generation; invalid recursive inline layouts are rejected;
  payload roots survive collection in every active case.

### T018 — Implement growable arrays, ordered maps, and ranges

- [ ] Depends on: T017.
- Add packed element storage and correct width/stride accesses, pre-sized
  literal allocation, basic methods, insertion order, iteration mutation
  detection, and overflow-free integer range execution.
- Done when: optional map values, missing lookups, equality/hash invariants,
  signed zeros/NaNs in keys, and maximum-endpoint loops behave as specified.
  Literal evaluations create independent mutable collections.

### T019 — Implement closures and captured variables

- [ ] Depends on: T015, T017, T018.
- Add capture classification, closure environments, shared mutable cells as
  the initial implementation, fresh loop captures, and noncapturing functions.
  Keep open upvalues as a measured alternative.
- Done when: callbacks outlive their source invocation safely, shared captures
  observe mutations, noncapturing functions allocate no closure object, and
  host-retained callbacks remain rooted until released.

### T020 — Implement results, recovery, and deferred cleanup

- [ ] Depends on: T017, T019.
- Finish `try`, mapping, `try?`, recovery, `E!void`, cleanup ordering, secondary
  faults, cleanup budgets, and the reserved-memory policy.
- Done when: returns, breaks, faults, and propagation run the required cleanup
  in order; looping/faulting/exhausted cleanup returns control and reports
  skipped work without hiding the primary failure.

## M2: complete the Stage 1 workload

### T021 — Complete core syntax and checking

- [ ] Depends on: T020.
- Complete lexical forms, named/default arguments, destructuring, optional
  chains, narrowing, guards, exhaustive payload/range patterns, labeled loops,
  methods, visibility, static members, and formatting/string helpers.
- Done when: non-generic core examples compile and execute; expected types,
  argument evaluation order, shadowing, and diagnostics match the reference.
  Add feature fixtures as each feature lands rather than one final test dump.

### T022 — Implement source modules and module analyses

- [ ] Depends on: T021.
- Add host-controlled loading, import graphs/cycle rejection, immutable exports,
  module instances, SCC-based return inference, and late-binding reachability.
- Done when: initialization happens once in dependency order; function-value
  references count in initialization safety; stress tests include functions
  and late bindings growing together; failed initialization has defined state.

### T023 — Implement declaration modules and runtime templates

- [ ] Depends on: T022.
- Parse/check host declarations, bind typed functions/properties/values once,
  and share immutable descriptors through templates. Keep instance state and
  per-runtime IDs/caches separate.
- Done when: mismatched bindings fail registration; source imports never
  reparse host declarations; fresh runtimes share descriptors without sharing
  mutable script module state.

### T024 — Complete the Stage 1 Rust embedding contract

- [ ] Depends on: T019, T020, T023.
- Implement thread-bound runtime APIs, checked lookup/calls, typed host
  objects, retention, stale handles, reentry, and context-borrowed views.
- Done when: compile-fail tests prevent views spanning reentry; nested runs
  share budget; handles from another runtime are rejected; destruction and
  panics/faults follow the documented contract.
- Carried from T010 (2026-11-08): reentry handles (`ScriptFn<'call>`) are
  valid only during the host call that found them, so hosts look a script
  function up by name on every call. Retained callback handles must not
  reintroduce stale identity (see the
  [ledger](implementation/vm-research.md#reentry-handle-identity-address-and-name-hash-abandoned--2026-11-08));
  measure per-call lookup against retained handles on a callback-heavy
  workload before choosing the retained representation.

### T025 — Build and document the Stage 1 editor corpus

- [ ] Depends on: T021, T023, T024.
- Create an explicit Stage 1 fixture derived from the reference config,
  replacing dynamic options with typed data and omitting runtime compilation
  and async features. Keep the full reference fixture unchanged as the target.
  Supply the equivalent Lua tree and instrumented host stubs.
- Done when: both programs produce equivalent recorded setup effects and
  retained callbacks; exclusions are listed, not hidden behind no-op bindings.

### T026 — Publish the Stage 1 baseline and CLI

- [ ] Depends on: T011, T022, T025.
- Complete the supported CLI/diagnostics and benchmark single/repeated runs,
  fresh source modules versus loaded dependencies, GC, allocations, metadata,
  and first callback invocations.
- Done when: commands reproduce the report; supported features and limitations
  are documented; results state whether targets were achieved. Stage 1 can
  complete even if the performance hypothesis needs revision.
- Progress 2026-11-07: the CLI exposes each stage: `run` and `check`
  (`.eit` or `.eits`), `asm [--annotate] [-o]`, `verify`, and
  `dump tokens|ast`, with `--host <path>=<file>` to register declaration
  files. `Context::assemble`, `Context::disassemble`, and
  `Context::dump_tokens|dump_ast` back them; the CLI does only file and
  process I/O.

### T048 — Track performance across targets

- [ ] Depends on: T011, T046.
- Refs: [Performance](implementation/performance.md); D24.
- Extend the harness to the full workload and stress-workload lists, run on
  Intel, AMD, and AArch64 machines with recorded models, and capture the
  counters the Method section requires (instructions, cycles, indirect-branch
  MPKI, µop-cache delivery where available). Store raw samples per commit.
- Done when: a report compares any two commits per workload and target with
  confidence intervals; a change can be attributed to a feature workload;
  regressions are flagged for review, never as failing unit tests.

### T049 — Implement the core numeric library

- [ ] Depends on: T013, T016.
- Refs: [Expressions: arithmetic](language/expressions.md#arithmetic),
  [Types: conversions](language/types.md#conversions),
  [Eitri and Lua: numbers and strings](lua.md#numbers-and-strings).
- Add the Lua parity numerics: power, `floor_div` and `floor_mod`, absolute
  value, min/max/clamp, float rounding (`floor`, `ceil`, `round`, `trunc`),
  `sqrt`, exponentials, logarithms, trigonometry, constants (`pi`, `inf`,
  `nan`, per-type min and max), and the documented number parsing
  (`i64.parse`, `f64.parse`) and exact formatting. Random numbers come from a
  host-provided module, so scripts stay deterministic unless the host opts in.
- Done when: integer helpers fault exactly where the language says; float
  functions match IEEE results (correctly rounded where the host library
  is); parse/format round trips are exact for every finite `f64` tested.

### T050 — Implement the core string, bytes, and UTF-8 library

- [ ] Depends on: T016, T018.
- Refs: [Strings](language/strings.md), [Collections](language/collections.md).
- Complete the built-in string methods in the Strings chapter, `bytes`
  access and slicing, and the UTF-8 outcomes of Lua's `utf8` library:
  validation, decoding at a byte offset, character counting, encoding a
  `char`, and iterating characters with byte offsets.
- Done when: every method in the chapter has fixtures, including empty
  strings, invalid UTF-8 in `bytes`, and non-ASCII case mapping; the
  Strings workload runs; Lua-style patterns are explicitly deferred to T052.

## M3: generics, interfaces, and foreign layouts

### T027 — Implement checked generics and bounded specialization

- [ ] Depends on: T026.
- Add generic declarations/aliases, constraints, expected-type inference,
  checked-once bodies, instantiation identity, and specialization limits.
- Done when: illegal polymorphic recursion and depth/count exhaustion produce
  diagnostics before setup effects; unused instantiations emit no code;
  compile time/code size are measured on combination-growth workloads.

### T028 — Implement interfaces and extensions

- [ ] Depends on: T027.
- Add explicit conformance, default methods, extension ownership checks,
  existential interface values, boxing, and indirect calls. Define which
  `Self`/generic/static requirements can be invoked through an existential.
- Done when: incoherent conformances and unsupported existential operations
  are rejected; boxed values are traced and dispatch matches static semantics.

### T029 — Implement extern layouts and checked host memory access

- [ ] Depends on: T024, T027.
- Add target-aware C-layout validation, storage types, extern arrays/classes,
  stale-handle checks, read/write permissions, checked stores, and explicit
  unsafe host obligations for pointer/string validity and aliasing.
- Done when: size/alignment/offset mismatches fail registration; narrow
  accesses never overread; pointer fields remain inaccessible to scripts;
  reentry cannot leave a safe Rust memory view alive.

### T030 — Implement the C API and binding generation

- [ ] Depends on: T028, T029.
- Add status/outcome APIs, mirror generation, handles/retention, templates,
  callbacks, reentry contracts, and diagnostics without cross-ABI unwinding.
- Done when: a C host executes the Stage 2 corpus; generated layout assertions
  catch drift; ownership and errors are tested in release as well as debug.
  Freeze ABI promises only after this implementation review.

### T051 — Implement the generic collection library

- [ ] Depends on: T027, T028.
- Refs: [Collections: array methods](language/collections.md#array-methods),
  [Generics: built-in constraints](design/generics.md#built-in-constraints).
- Write, as ordinary generic Eitri or built-in generic functions: sorting
  (stable, with `Ordered` or a comparator), binary search, map/filter/fold
  over arrays, reversing, range copy (Lua's `table.move`), and map
  key/value collection. These replace Lua's `table` library.
- Done when: sorting is stable and faults (not corrupts) when a comparator
  faults; helpers allocate only their result; specialization limits from
  T027 hold for the library's own instantiations.

## M4: stackful concurrency

### T031 — Implement async effects and movable coroutine stacks

- [ ] Depends on: T026.
- Add async typing, sync-to-async conversion, stackful tasks, suspension tokens,
  frame relocation, and sync callback barriers. Root suspended frames.
- Done when: suspension never crosses a native host frame; stack growth
  preserves captures/roots; stale/doubled tokens are host API errors.

### T032 — Implement joining, cancellation, fault containment, and reports

- [ ] Depends on: T020, T031.
- Add terminal states, `Stopped!T`, FIFO waiters/ready queue, cancellation at
  awaits, shared budgets, cleanup, host reports, and explicit drain behavior.
- Done when: reentrant start/resume/cancel, completion races, cleanup faults,
  and budget-exhausted queues match the documented state machine.

### T033 — Implement channels and async binding APIs

- [ ] Depends on: T030, T032.
- Add channel send/receive/close, wakeup ordering, optional-value semantics,
  async host callbacks, and operation cancellation hooks.
- Done when: FIFO/close/cancellation tests pass and coroutine cost measurements
  include ordinary nonsuspending-call overhead. Stage 3 gate is recorded.

## M5: dynamic and runtime features

### T034 — Implement dynamic values, type patterns, and JSON

- [ ] Depends on: T028, T030.
- Add `dyn` representation/boxing, typed extraction, dynamic reads/writes/calls,
  visibility-aware descriptors, tracing, and host-enabled JSON facilities.
- Done when: missing data, misuse faults, exact type tests, NaN behavior,
  and nested boxed/reference values match the language. Begin without caches.

### T035 — Implement weak references and queued collection cleanup

- [ ] Depends on: T015, T033, T034.
- Add weak references, ephemeron weak-key maps, safe-point finalization queues,
  and host drain policy without running script code inside collection.
- Done when: backreferences do not accidentally retain ephemeron entries;
  callbacks cannot receive resurrectable collected objects; cleanup failures
  are independently reported and bounded.

### T036 — Implement runtime compilation and the full reference config

- [ ] Depends on: T033, T034, T035.
- Add host-enabled `compiler.load`, expected-signature checking, capability-
  limited imports, loaded-code ownership, and initialization outcomes.
- Done when: the complete editor fixture and equivalent Lua tree execute;
  invalid loaded source has no setup effects; loaded closures survive
  collections. Record Stage 4 feature coverage and the remaining library work.

### T052 — Decide and implement the string pattern library

- [ ] Depends on: T050, T051.
- Refs: [Strings: methods](language/strings.md#methods),
  [Open questions: Lua parity](open-questions.md#lua-parity).
- First record a decision: Lua pattern syntax, or a regular-expression
  subset, and what matching guarantees (linear time, or charged to the
  execution budget). Then implement find, match with captures, global
  iteration, and substitution.
- Done when: the decision is in the Strings chapter; every Lua pattern
  outcome used by the Lua equivalent of the reference config has an Eitri
  fixture; a pathological pattern returns control through the budget.

### T053 — Provide reference system modules as host modules

- [ ] Depends on: T024, T030, T033.
- Refs: [Embedding: host modules](implementation/embedding.md#host-modules),
  [Eitri and Lua: embedding and runtime](lua.md#embedding-and-runtime).
- Ship optional declaration files and Rust/C implementations for the
  outcomes of Lua's `io` and `os`: files, time and clocks, environment
  variables, and process exit status, with async variants where the
  operation blocks. They are never imported by default: the host decides
  which ones a runtime can see.
- Done when: a runtime without them cannot reach any of their effects;
  each module works from both Rust and C hosts; blocking operations have an
  async form that suspends instead of blocking the host thread.

### T054 — Implement GC control and memory introspection

- [ ] Depends on: T015, T035.
- Refs: [VM: garbage collection](implementation/vm.md#garbage-collection),
  [Eitri and Lua: embedding and runtime](lua.md#embedding-and-runtime).
- Give hosts the outcomes of `collectgarbage`: a full collection, an
  incremental step if the collector has one, memory statistics (heap,
  retained compiler metadata, code), and limit changes. Expose a script
  module for these only when the host enables it.
- Done when: statistics reconcile with T015's accounting; a forced
  collection from the host during reentry is safe; a script without the
  module cannot trigger collection directly.

### T055 — Implement debug introspection

- [ ] Depends on: T024, T031.
- Refs: [Embedding: faults and traces](implementation/embedding.md#faults-and-traces).
- Give hosts the outcomes of Lua's `debug` library that the parity table
  lists: tracebacks across script, host, and task frames; reading the
  locals and arguments of a suspended or faulted frame through debug
  metadata; and function and source information. Execution hooks remain
  the budget mechanism; breakpoints are out of scope.
- Done when: traces name every frame kind correctly, including tasks and
  reentrant host calls; reading locals never observes a dead or reused slot
  as live (it uses the same liveness as the root maps).

### T056 — Audit Lua parity

- [ ] Depends on: T036, T051–T055.
- Refs: [Eitri and Lua](lua.md).
- For every row of the parity tables, write an Eitri fixture that achieves
  the outcome and the Lua program it replaces. A row without a working
  fixture is a gap, reported in Open questions, not a footnote.
- Done when: every ✅, 🔁, and 📚 row has a passing fixture pair, the 📚 rows
  name the module that provides them, and the Stage 4 gate records the
  result.

## M6: optimize measured costs

Each task ends with a retained/rejected/deferred decision and reproducible
data. These tasks do not authorize replacing the correctness baseline first.

### T037 — Optimize lowering and slot allocation

- [ ] Depends on: T012; repeat on T026 when available.
- Compare lazy value materialization, constant/immediate selection, copy
  coalescing, and short-operand slot placement against simple lowering (E2/E3).
- Done when: differential semantics pass and reports include compilation time,
  executed moves, `WIDE` frequency, frame size, allocations, and first result.
- Progress 2026-11-08: optimizations needing analysis run in a hot tier,
  not in first-run lowering. `Context::optimize` re-lowers hot code with
  its logical instructions recorded (a compile-time instantiation of the
  assembler, so first runs are unchanged: compile within noise of
  `bf5b64c`), runs `eitri_bytecode::opt` (argument-copy coalescing,
  loop-invariant literals; liveness bounded by `ANALYSIS_LIMIT` and
  `SCAN_LIMIT`), re-assembles, and verifies. Every fixture, limits program,
  random program, and mutated `.eits` runs at both tiers with identical
  outcomes; unit tests cover each pass's conditions, and removing any one
  fails them. Steady-state execution: `arithmetic` −15%, `host_calls` −14%,
  `branching` −8%, break-even after 4–7 runs; `calls` and `module` gain
  little or nothing
  ([ledger](implementation/vm-research.md#hot-tier-re-optimizing-bytecode-from-recorded-eits--2026-11-08)).
  Audit fix 2026-11-08: hoisting allocated `%r65536` in a frame already
  using every encodable slot; it now keeps the load when no slot is left
  (`opt_tests::literals_stay_when_no_slot_is_left`).
  Open: reports of executed moves and `WIDE` frequency, and more passes.

### T038 — Optimize literal construction and execution metadata

- [ ] Depends on: T018, T025.
- Compare pre-sized constructors and compact initialization paths (E14), plus
  hot/cold call-site splitting and shared root maps (E13).
- Done when: source effects/identity/fault ordering are preserved; report
  allocations/reallocations, total artifact bytes, call-handler loads, and GC cost.

### T039 — Compare dispatch, register caching, and measured fusion

- [ ] Depends on: T026, T037.
- Run E5/E11/E12 with pinned compilers, inspected assembly, and corpus-derived
  candidates. Distinguish canonical bytecode opcode space from private
  execution-handler variants; cache only scalars initially.
- Done when: spills/joins/safepoints preserve state; handler footprint and
  compilation cost accompany latency/counter results; target-specific wins
  are labeled rather than generalized across CPUs.

### T040 — Compare root production and collector representations

- [ ] Depends on: T026.
- Run E7: precise live maps versus typed-slot lifetime maps and conservative
  frame scanning, with explicit initialization and pointer-validity rules.
- Done when: collection stress passes; report false retention, root metadata,
  compile time, frame footprint, allocation-failure behavior, and pause tails.

### T041 — Implement and evaluate dynamic site caches

- [ ] Depends on: T034.
- Implement complete guards and epoch-aware reentrant state joins, then run
  E6 against uncached operations. Compare side caches first; private-code
  rewriting remains optional and separately accounted.
- Done when: same-type/different-map layouts, invalidation, megamorphism,
  reentry, and GC ownership pass differential tests; total setup/hit/miss
  costs and retained cache memory are reported.

### T042 — Evaluate lazy function lowering

- [ ] Depends on: T026, T037.
- Run E10 using retained checked bodies/capture metadata separate from scratch.
  Keep eager lowering available and charge first-callback compilation fully.
- Done when: diagnostics remain complete before setup; retained bytes,
  first-result/first-callback latency, and total-workload time determine
  promotion under the ledger's stated gate.

### T043 — Prototype baseline native execution and optional AOT

- [ ] Depends on: T026, T039.
- Run E1/E8/E9 on the same semantic subset: interpreter/checked-tree candidate,
  a small baseline emitter, and a copy-and-patch candidate as resources permit.
  Reuse canonical VM state at safepoints and preserve budgets/fault PCs.
- Done when: differential checks pass; first-use and reused code-memory costs,
  publication/cache synchronization, native bytes, runtime binary size, and
  break-even execution counts are published. AOT results identify linking,
  target/ABI, verification/trust, and reload constraints separately from the
  fresh-source target. No mandatory native tier without a measured win.

### T057 — Compare lexing, interning, and syntax storage

- [ ] Depends on: T012; repeat on T026.
- Refs: D2, D3, D18; E2.
- Run E2: intern while lexing versus while resolving, a token buffer versus
  streaming, and a SIMD scanning path for identifiers, whitespace, and
  string bodies.
- Done when: results are reported per source byte, by input size, with
  allocation counts and the long-string and nested-lambda cases; the SIMD
  path is kept only if it wins on the reference config, not only on large
  inputs.

### T058 — Compare call frame models

- [ ] Depends on: T026.
- Refs: D8; E4; [Execution design: frame and call model](implementation/execution-design.md#frame-and-call-model).
- Run E4: separate copied call frames versus shared call windows with
  liveness checks.
- Done when: end-to-end latency, argument and result moves, frame growth,
  nested calls, and host reentry are reported; suspension and root maps stay
  correct under both models.

### T059 — Evaluate identity, hashing, and map representations

- [ ] Depends on: T026, T040.
- Refs: [Identity and hashing: experiments](implementation/identity.md#experiments),
  [map representations](implementation/identity.md#map-representations).
- Run the identity experiments: a moving collector with header hash states,
  eager interning of short strings, and specialized map representations.
- Done when: the identity invariants hold under each candidate (forced
  collection between hashing and lookup included), and memory, lookup, and
  collection costs are reported against the first implementation.

### T060 — Evaluate closure representation and generic shape sharing

- [ ] Depends on: T019, T027.
- Refs: [VM: closures](implementation/vm.md#closures),
  [Generics: specialization](design/generics.md#implementation-specialization);
  [R27](implementation/vm-research.md#r27-specialization-versus-shared-generic-code).
- Compare heap cells with open upvalues for captured variables, and full
  specialization with sharing code between instantiations of the same shape.
- Done when: captured-variable semantics, suspension, and collection are
  unchanged; closure creation and call costs, code bytes, and compile time
  are reported; shape sharing is kept only if its saving is measurable.

## Later: reload and release hardening

### T044 — Settle and implement reload, then freeze release contracts

- [ ] Depends on: T036; incorporate any promoted T037–T043 representation.
- Resolve state retention, folded constants, generic copies, caches, closures,
  old frames, and code reclamation before implementing reload. Define
  transactional publication and initialization-failure behavior.
- Done when: rejected reloads preserve published state under the chosen
  contract; old frames/callbacks remain valid; invalidation is tested.
  Review public Rust/C APIs and release feature/library coverage before
  declaring ABI stability or full Lua capability coverage.

### T061 — Settle and implement the grammar extensions

- [ ] Depends on: T029.
- Refs: [Open questions: not yet in the grammar](open-questions.md#not-yet-in-the-grammar).
- Decide and implement attribute arguments (`@name(...)`), packed
  `@extern` layouts and explicit offsets, and enum field storage widths
  other than `i32`. Explicit type arguments in expressions stay deferred
  unless a decision changes that.
- Done when: the grammar, the Structs chapter, and `eitri-bindgen` agree;
  packed layouts are checked against the C compiler's layout on every
  target the layouts are run on.

### T062 — Decide the convenience features

- [ ] Depends on: T056.
- Refs: [Open questions: convenience, not parity](open-questions.md#convenience-not-parity).
- Write a decision record for each: tuples or multiple returns, varargs,
  operator overloading, mixed integer and float arithmetic, circular
  imports, runtime-wide globals, and generators. Each record weighs the
  porting benefit against cost on the source-to-result path.
- Done when: each feature is accepted with a design chapter and tasks, or
  rejected with its reason in Open questions. None is implemented before
  its record.

### T063 — Package and release

- [ ] Depends on: T044.
- Define versioning for the language, the Rust crate, and the C ABI
  separately; a minimum supported Rust version; the license; the published
  crates (splitting the crate only where build time or API boundaries
  justify it); the C header and library packaging; and a changelog.
- Done when: a release can be produced from a tag by one documented command,
  and the C ABI version follows the rules in the C API chapter.

## Continuous

### T064 — Security and resource-limit review

- [ ] Depends on: T012; repeat at the M2 and M5 exits.
- Refs: [Embedding](implementation/embedding.md), [Rust API](implementation/rust-api.md),
  [C API](implementation/c-api.md).
- Maintain a threat model for untrusted script source: what a script can
  reach without host imports (nothing outside its runtime), limits on
  memory, execution budget, call depth, task count, and compiler recursion,
  and pathological inputs (deep nesting, huge literals, long identifiers,
  combinatorial generics). Fuzz the lexer, parser, checker, and assembler;
  audit `unsafe` code.
- Done when: every limit has a fixture that hits it and returns control to
  the host; fuzzers run through a documented command; findings are fixed or recorded
  with their accepted risk.

- M0 review 2026-11-12: [threat model and audit](implementation/security.md)
  cover source, `.eits`, capabilities, host calls/reentry, all eight production
  unsafe blocks, algorithmic bounds, and the limits exercised by the corpus.
  Added generated adversarial tests and zero-budget/zero-depth fixtures;
  corrected the hash module's non-adversarial-input assumption. No compiler,
  lowering, numeric, literal, frame-layout, or VM behavior changed.
  Remaining risks are explicitly recorded: no M0 memory/compiler-work quota,
  deterministic hash flooding, verifier block-by-frame growth, inspection-only
  huge input/index limits, assembly span narrowing and trusted host result bits.
  Finding 4 is fixed by enforcing same bindings: artifacts record only used host indexes with owned module/function
  names and checked signatures; execution rejects registry mismatches with
  typed E0003 before effects or run-count changes. Identically bound contexts
  remain compatible (T023). Validation allocates nothing on success and costs
  O(used bindings), including name bytes and signature parameters. Regression
  tests cover mismatches and cross-context success for source/simple/assembly,
  both interpreters and optimized artifacts. Finding 6 is fixed: threaded
  call growth checks fuel/depth before changing slot/frame storage, with the
  hot path unchanged and debit/fault precedence preserved. Switch execution
  and host reentry were already ordered correctly. VM unit tests probe private
  capacities on both interpreters for denied depth/fuel calls and host reentry;
  caught refusals preserve spent fuel. T011 before/after measurements and check
  results are recorded in `/tmp/orchestra/eitri-3-done.md`. T064 remains
  continuous; this is an M0 review with recorded limitations, not a complete
  denial-of-service sandbox.
- M0 checks: locked workspace build, formatting check, Clippy with warnings
  denied, and workspace tests pass. Focused security (6), limits (5), fixture,
  diagnostics, lifecycle and verifier-memory tests pass. Seeded release fuzzing
  passes 100,000 cases each for source, assembly and bytecode. Miri passes
  20 lexer tests, the parser nesting test, six VM tests and the caught-reentry
  test; broad syntax attempts were interrupted in expensive stress cases
  (not reported as passes). Catalog and 445-link documentation checks pass.
  The 135-source lifecycle run retained 423,706/554,778 bytes (default
  match/threaded) and 423,686/554,758 (simple), stable across 20 warm rounds;
  no latency or memory improvement is claimed. Full output and review
  limitations: `/tmp/codex-eitri-par-T064.md`. T048 awaits the requested review.

### T065 — Keep the documentation true

- [ ] Depends on: none; runs with every task.
- When behavior lands, update its chapter; remove resolved ⚠️ markers;
  move settled questions from Open questions into chapters; record
  experiment outcomes in the ledger; keep the diagnostic catalog (T047) and
  the opcode manifest (T003) generated rather than hand-edited.
- Done when (each milestone exit): no chapter describes behavior the
  implementation contradicts, and the internal link check passes.

- M0 exit check 2026-11-12 (T047 step 2): the diagnostic catalog now has
  explicit generation/check commands and snapshot updates require review.
  M0 diagnostic/recovery documentation reflects the existing implementation;
  `python3 tools/check_doc_links.py` checks local inline Markdown links and
  ATX-heading fragments across AGENTS.md, README.md, and docs. External URLs
  and fenced examples are excluded. The checker catches deliberately broken
  target/fragment controls and accepts duplicate-heading fragments. T065
  remains continuous for later milestones; no future language features are
  claimed implemented by this gate.

## Traceability

### Ledger decisions and experiments

| Decisions | Implemented in | Measured in |
|---|---|---|
| D1 typed register bytecode | T005, T006 | T043 (E1) |
| D2, D3, D18 lexing, interning, scratch, SIMD | T007, T008 | T057 (E2) |
| D4 signatures before bodies | T008 | — |
| D5, D22, D37, D38 typed operations, copy coalescing, lazy materialization, slot allocation | T009 | T037 (E3) |
| D6, D13, D28, D36 encoding, generated specification, families, manifest | T003, T005 | T039 (E11) |
| D7, D21 slot width and full-width stores | T006, T013 | T037 (E3) |
| D8 control frames and call windows | T006 | T058 (E4) |
| D9, D19, D20, D24 dispatch and handler rules | T006 | T039 (E5) |
| D10, D42 fusion | — | T039 (E5) |
| D11, D23 dynamic site caches | T034, T041 | T041 (E6) |
| D12 precise safepoints and roots | T014, T015 | T040 (E7) |
| D27 conservative frame scanning | — | T040 (E7) |
| D14–D17, D25 native tiers and AOT | — | T043 (E1, E8, E9) |
| D26 lazy lowering | — | T042 (E10) |
| D29–D32 field positions, signedness, canonical integers, compares | T003, T006, T013 | T039 (E11) |
| D33 `SEL` | — | T039 (E11) |
| D34 load/store and access widths | T017, T018 | T039 (E11) |
| D35 pair move and range loops | T006, T018 | T039 (E11) |
| D39 register-cached temporaries | — | T039 (E12) |
| D40 pre-sized literals | T018 | T038 (E14) |
| D41 metadata layout | T014 | T038 (E13) |

### Language and design chapters

| Chapter | Tasks |
|---|---|
| [Lexical structure](language/lexical-structure.md) | T007, T021; attributes T029, T061 |
| [Types](language/types.md) | T002, T013, T021 (aliases), T022 (return inference) |
| [Bindings and functions](language/bindings-and-functions.md) | T008, T019, T021, T031 |
| [Structs, classes, enums](language/structs-classes-enums.md) | T017, T021, T028, T029 |
| [Optionals](language/optionals.md) | T017, T021 |
| [Errors and faults](language/errors.md) | T006, T012, T020 |
| [Pattern matching](language/pattern-matching.md) | T017, T021, T034 (type patterns) |
| [Collections](language/collections.md) | T018, T051 |
| [Expressions](language/expressions.md) | T008, T013, T018, T021, T049 |
| [Strings](language/strings.md) | T016, T050, T052 |
| [Modules](language/modules.md) | T022, T023 |
| [Generics](design/generics.md) | T027, T028, T060 |
| [Dynamic type](design/dynamic.md) | T034, T041 |
| [Coroutines](design/coroutines.md) | T031, T032, T033 |
| [Runtime features](design/runtime-features.md) | T033, T035, T036, T044 |
| [Embedding](implementation/embedding.md), [Rust API](implementation/rust-api.md) | T010, T023, T024, T033 |
| [C API](implementation/c-api.md) | T030, T053 |
| [Identity and hashing](implementation/identity.md) | T016, T017, T018, T059 |
| [Typed bytecode](implementation/bytecode.md) | T003, T005, T006, T013, T018 |
| [Performance](implementation/performance.md) | T011, T026, T048 |
| [Eitri and Lua](lua.md) | T049–T056 |

## Risks and replanning triggers

| Trigger | Response |
|---|---|
| E1 (T043) shows checked-tree or direct native execution beating bytecode on first result | Revisit T005–T009 before M2; the ledger allows E1 to change the default path |
| The generated manifest (T003) leaves too little opcode space for M3–M5 families | Merge families or move rare variants behind a second-level opcode, before adding features |
| The Stage 1 baseline (T026) misses the latency target by a wide margin | Profile and fix the largest cost before starting M3 |
| Precise root maps (T014) dominate compile time | Bring T040 forward and compare conservative frame scanning |
| Movable coroutine stacks (T031) conflict with host borrowing or callbacks | Spike T031's stack relocation against T024's borrowing rules during M2 |
| Pattern matching (T052) cannot be made budget-safe | Choose the linear-time design even if it differs from Lua's syntax |

## Out of scope until a decision

Generators, tuples, varargs, operator overloading, circular imports, and
runtime-wide globals (T062); explicit type arguments in expressions (T061);
a debugger with breakpoints (T055); a mandatory native tier (T043).

## Immediate next task

**T014 — define layouts and root descriptors**. T013 scalar representations
and conversions are complete. T014 has not started.
