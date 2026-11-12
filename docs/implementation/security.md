# Security and resource review

T064, M0 exit review, 2026-11-12. This review covers the scalar slice on
AArch64 macOS. Repeat it at M2 and M5; it does not certify future features.
The normative M0 resource rules remain in [First slice](first-slice.md#resources).
The general [embedding](embedding.md), [Rust API](rust-api.md), and
[C API](c-api.md) chapters describe later facilities as well.

## Trust boundary

Source and `.eits` text may be hostile. With no registered host modules,
a script has no filesystem, network, environment, clock, process, or other
runtime access. Import paths are registry keys, never filesystem lookups
in the library. Compilation rejects unsupported features before effects.
Assembly resolves declared host names and signatures against that same
registry and verifies every function before returning executable code.

The embedding Rust host is trusted: it controls registration, limits, host
implementations, compiled artifacts, and raw scalar argument/result slots.
`Context::execute` requires code from the same context; this is documented,
but is not enforced by a runtime identity. Passing code between registries
can dispatch the wrong host index. Do not pass artifacts between contexts.
The low-level bytecode/VM APIs likewise require callers to validate code;
unverified execution can panic or abort. They are not an untrusted loader.

Host work is outside fuel accounting. A blocking or allocating host function,
a host panic, invalid host result bits, or host misuse of call windows is not
contained by the VM. Reentry validates argument/result counts and canonical
boolean arguments, borrows the active machine, and shares fuel/depth. It
cannot restart module initialization. Nested faults may be caught; spent
fuel remains spent. A `ScriptFn` cannot be retained beyond its host call
(compile-fail lifetime tests). C ABI, native artifacts, managed values,
GC, async tasks, and runtime compilation are absent at M0.

The CLI is a separate trusted file-I/O wrapper. It reads the input and
auto-discovers ancestor `.eith` declarations. These do not supply host
implementations: calling them faults. The CLI is not a process sandbox.

## Limit coverage

| Resource | M0 bound and evidence |
|---|---|
| Execution | Default 1,000,000 debits; configurable down to zero. Calls and taken branches to the same/earlier instruction debit before continuing. Source loop/call fixtures cover exact and exhausted fuel; `security::assembly_cycles_exhaust_budget_in_every_tier` covers unconditional, boolean, integer and immediate self loops in both interpreters before/after optimization. Straight-line operations are not individually charged. |
| Script frames | Default 4,096, configurable to zero, module excluded. `stack_overflow`, `zero_depth`, `reentry_depth` fixtures; VM unit test also executes 200,000 recursive calls without native recursion. Frame bytes are not capped. |
| Native reentry | 64 nested runs. `limits::nesting_limit` runs 64 and rejects 65. `reentry_budget*`, `reentry_fault`, `reentry_signature` fixtures and `security::caught_reentry_fault_does_not_reset_budget` cover shared limits and propagation/recovery. |
| Compiler nesting | 256 delimiter/syntax levels. Syntax counts the leaf expression: 255 surrounding parentheses pass; 256 reject. Lexer/parser boundary tests and generated security cases also reject 257 delimiters/blocks and 10,000 prefixes before any host effect. Flat 10,000-operation expressions execute. |
| Locals/encoding | 65,535 locals; `limits::locals_limit` runs the boundary and rejects one more. The same target exercises wide slots, far branches and constant indexes through both VMs, oracle, and assembly round trips. Temporary-slot exhaustion remains a lowering limit, not a license to change layouts. |
| Source/index width | Source at least 4 GiB is rejected by the source lexer before tokenization. Not allocation-tested: constructing that input is intentionally excluded from the quick suite. Distinct-symbol/index exhaustion is also impractical to exercise. `.eits` spans currently narrow byte offsets without the source lexer's size guard; host must bound assembly input well below 4 GiB. |
| Diagnostics | Context retains at most 100 reports, with per-stage 1/99/100/101/300 error tests in `diagnostics`. Capping stored reports does not bound all work or allocation while constructing a report. |
| Memory | No M0 allocation quota, fallible allocator, compiler scratch cap, code cap, or frame-byte cap. `lifecycle` checks stable retained high-water on repeated corpus runs and full release at context drop; `verify_memory` checks one wide straight-line function's peak, not a general memory bound. |
| Tasks/generics | Absent. `spawn` and generic declarations are rejected before effects by unsupported fixtures. Re-audit task count and specialization expansion when these land. |

`security.rs` uses generated inputs where storing enormous fixtures would
obscure review: 128 KiB identifiers/comments/literals, flat expressions,
nesting, malformed frames/slots/targets, capability rejection, and context
reuse after rejection. Small zero-budget and zero-depth boundaries use the
normal discovered `.eit`/`.expect` format. Existing corpus expectations,
independent program oracle and two interpreters remain the semantic checks.

## Algorithms and remaining findings

Let B be source/text bytes, I instructions, F frame slots, and K basic blocks.
Lexer scanning and token/AST storage are O(B) under ordinary hash behavior;
parser nesting consumes O(256) native frames. Checking and lowering walk
flat expression chains iteratively. Interning uses deterministic Fx hashing
and linear probing: worst-case collision attacks can require quadratic
work in the number of distinct names. Hash tables in checking/assembly have
the same denial-of-service concern. There is no compiler fuel/deadline.
The prior `fx.rs` claim that inputs are not adversarial was incorrect.

Assembly tokenizes and makes two header/body passes, using O(B) storage;
branch relaxation and verification add their own costs. The verifier retains
O(I + K*F) storage: a wide frame plus many blocks can exhaust memory despite
the earlier straight-line memory fix. Its fixed-point work revisits blocks
as slot facts degrade; the module header's simple O(I*F) statement should
not be treated as an adversarial time guarantee (joins themselves scan F
slots, and blocks may be revisited). Optimizer liveness is capped by
`ANALYSIS_LIMIT`; copy scans stop at `SCAN_LIMIT`, but these are not caps on
assembly or verification.

M0 risk disposition: the review records these existing limitations rather
than promising a complete in-process denial-of-service sandbox. Hosts taking
untrusted input must bound input sizes and registration/artifact counts and
use external process resource limits when hard time/memory containment is
required. No new numeric, literal, lowering, or layout semantics were added.
Follow-up work:

- T015/T064 M2: account compiler scratch, retained code/metadata, heap and
  frame bytes, and return allocation exhaustion to the host. Exercise every
  introduced quota at and over its boundary.
- T064: bound verification state/work and compiler hash-collision work;
  validate source-size/index-width handling in `.eits`. Huge-input and
  distinct-index limits remain inspection-only in this review.
- T010/T024/T064: enforce or strengthen the same-context compiled-artifact
  contract and host scalar result validation before broadening host values.
- T013/T014 coordination finding: threaded `CALL.SCRIPT` grows frame/slot
  storage before checking fuel/depth; a refused call can therefore allocate.
  Record frame-byte/growth policy with the other agent rather than changing
  VM layouts or call semantics in this worktree.

These are implementation/hardening findings, not unsettled language choices.
No specification decision was needed for this review.

## Unsafe audit and reproducible checks

Production unsafe is confined to `eitri-syntax/src/token.rs` (one block,
three token columns) and `eitri-vm/src/threaded.rs` (seven blocks, frame
pointer construction/access/reconstruction). Token columns share a checked
minimum capacity, equal lengths, initialized Copy elements, and no fallible
operation between stores and length updates. Threaded slot access checks
frame length; pointers are re-derived after stack growth and host reentry,
and caller/callee frames occupy the same live allocation. Each block has a
SAFETY comment. E2/E5 measurements in the [ledger](vm-research.md) explain
the removed checks/dispatch costs. No unsafe code was changed or added.
Test/benchmark allocator wrappers forward layouts/pointers to `System`;
their process-global atomic counters are isolated measurement instrumentation,
not compiler/runtime state. There is no C implementation to sanitize yet.

```sh
cargo test --locked -p eitri --test security --test limits --test fixtures --test diagnostics --test lifecycle --test verify_memory
EITRI_FUZZ_CASES=100000 cargo test --locked --release -p eitri --test fuzz -- --nocapture
cargo miri test -p eitri-syntax -- lexer::tests:: --skip differential --skip adversarial
cargo miri test -p eitri-syntax -- parser::tests::nesting_limit_protects_the_stack
cargo miri test -p eitri-vm -- recursion calls_and faults host_calls scratch bool_not
cargo miri test -p eitri --test security caught_reentry_fault_does_not_reset_budget
python3 tools/check_doc_links.py
```

Fuzz targets are `tests/support/fuzz.rs`: arbitrary source bytes through
lexer/parser/checker/lowering and the independent oracle, `.eits` text through
assembly/verification and both tiers, and mutated code words through the
verifier and interpreters. `tests/fuzz.rs` runs deterministic seed ranges
from zero and prints accepted counts; failure reports include seed/input.
This is mutation fuzzing, not a coverage-guided campaign or proof of safety.
The final M0 run passed 100,000 mutations per target (10,437 sources compiled,
2,934 assembly texts assembled, 5,359 bytecode mutants verified and ran).
Miri passed the 20 selected lexer tests, parser nesting test, six VM tests,
and the caught-reentry integration test. Broader syntax Miri attempts were
interrupted in expensive scalar-classifier/parser stress cases; these were
not passing full runs. The native workspace suite includes those cases.
Raw command output for this review is retained with the T064 handoff report
at `/tmp/codex-eitri-par-T064.md` and its neighboring logs. No production
execution-path change was made, so no new latency/memory improvement is
claimed and the performance harness rerun is not required for this change.
