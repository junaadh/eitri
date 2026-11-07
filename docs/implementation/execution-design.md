# Source-to-execution architecture

Status: proposed first implementation, not a measured optimum. Every major
choice maps to the [research and decision ledger](vm-research.md). Exact
formats and thresholds are experiment parameters. This proposal refines the
[compiler](compiler.md) and [VM](vm.md); it does not change language semantics.

## Layer boundaries and context ownership

Keep source/diagnostics, frontend, typed operations/bytecode, VM, embedding,
and CLI responsibilities separate, with explicit interfaces and dependencies.
The CLI owns file/process I/O; core layers do not depend on the CLI. Place
independently implemented layers in workspace crates as they land.

The host owns the context and passes it explicitly through the operation.
Layers borrow their relevant state; compiler scratch can be reused without
global caches. Execution uses exclusive mutable access, so no locks or atomic
reference counting are required on this path. Independent contexts share no
hidden mutable state. Process globals, thread-local runtimes, registries, and
interners are prohibited; immutable constants/read-only generated tables are
fine. Reentry uses the active context and budget, preserving the embedding
lifetime rules. Host-driven concurrency must not introduce hidden locks into
ordinary compilation and execution.

The bootstrap exposes `Context::run(&mut self, source)`, still returning a
placeholder diagnostic. Layer implementations and their context fields are
not implemented yet; the API remains experimental.

## Optimize the complete operation

For N executions of a submitted program:

```
T(N) = T_frontend + T_lower + T_link + T_publish + N * T_execute
```

Measure reclamation and GC over repeated submissions too. For a common
frontend, native compilation wins over interpretation only if:

```
extra native setup + profiling overhead < N * (interpreter time - native time)
```

This is an accounting model, not a predicted threshold. Native setup includes
code allocation, relocation, executable-memory permission transitions,
instruction-cache synchronization, and metadata. A runtime may reuse a code
pool, but first use and replenishment remain reported costs. [D14–D16]

The first executable representation should be compact typed bytecode. It
also acts as input to a baseline native backend. Retain a checked-tree
experiment and a direct-to-native lowering experiment: bytecode is not a
logical necessity and need not be materialized on a native-only path. [D1]

```
source
  -> lexical tokens + spelling IDs + delimiter index
  -> compact syntax + declaration/signature tables
  -> checked bodies + type/layout/binding IDs
  -> typed operations + slots + effect/root metadata
       -> compact bytecode -> interpreter
       -> baseline native code
       -> optional offline AOT artifact
```

These are logical responsibilities, not seven compulsory full-tree passes.
Resolution, checking, constant propagation, slot assignment, and emission can
share a body traversal once their dependencies are available. A streaming
typed-operation sink can target bytecode or native code. Keeping every
intermediate representation alive is unnecessary. [D3–D5]

## What lexing should already accomplish

Use a compact token buffer for the first candidate. Each token has a kind,
source offset/length, and optional payload index. Hash identifier spellings
while scanning; an interner compares bytes on collisions and assigns a dense
`SymbolId`. Repeated uses carry that ID rather than new strings. [D2, D3]

Maintain a delimiter stack and record matching token indexes. Parser
lookahead for lambdas can jump to a closing parenthesis rather than repeatedly
rescan a nested body. Strings/interpolation must follow lexer state; braces
inside literal text are not syntax delimiters. This particular index is an
Eitri engineering hypothesis under E2, not a result established by R11.

Do not eagerly convert every numeric token to one machine type: integer
literals require expected-type range/exactness checks. Keep a spelling or a
validated constant payload sufficient for those checks. Decode escaped
strings when necessary and retain correct source spans. [D4]

Selective SIMD scanning is an alternative for long runs of whitespace,
comments, and string bytes. It must not impose a large minimum-input or
padding cost on short scripts. [D18]

Lexing creates spelling identity, not variable identity. Two shadowed `x`
bindings share a `SymbolId` but have different `BindingId`s. Nor can lexing
know whether `obj.x` is a fixed field, a host property, or dynamic access.
Do not execute script code while scanning or checking: the existing language
requires the submitted script to be checked before any setup effect. [D4]

## Tables that remove work from execution

Keep distinct typed IDs and specify their domains:

| ID | Domain and purpose | Replaces during execution |
|---|---|---|
| `SymbolId` | Compilation spelling interner; bytes retained for diagnostics/linking | Repeated spelling allocation; not all semantic lookup |
| `BindingId` | Lexical declaration in one module/body | Scope/name search |
| `TypeId` | Canonical runtime type identity; imported descriptors translated at linking | Repeated type-shape construction |
| `LayoutId` | Storage layout and tracing recipe | Recomputing field offsets or root structure |
| `FunctionId` | Runtime-owned stable function entry | Function-name lookup |
| `HostFnId` | Registered typed callback entry | Host-name lookup |
| `ConstId` | Chunk-owned immutable constant pool | Reconstructing constants |
| `AccessSiteId` | Function-owned access descriptor and optional cache index | Repeated dynamic resolution on hits |
| `RootMapId` | Function metadata | Reconstructing live roots at safepoints |

These are not interchangeable integers. Runtime identities must be unique
while any referring code, cache, or object lives; generation checks or
non-reuse policies prevent stale IDs. IDs stored in templates or artifacts
are translated into each runtime's domain. Hash values are never IDs without
collision checking. [D2, D11]

Build scope heads indexed by compilation-local symbol ID, with an undo stack
on scope exit, as the initial lookup candidate. This data-structure choice
is tested in E2; research about canonicalization does not prove it fastest.
Avoid arrays sized to every symbol ever seen by a long-running runtime.

Keep published artifacts separate from resettable compiler scratch. Code,
constants, source/debug information needed later, and tracing descriptors
must survive returning a callback from the submitted chunk. On failure,
discard unpublished artifacts without exposing incomplete entry tables.
This is not a rollback guarantee for later runtime initialization. [D3]

## Specialize from facts before speculating

The checker selects `ADD.CHK.S`, `ADD.WRAP`, or `FADD.F64`; normal
typed arithmetic never starts as a generic dynamic add and then warms up.
Fixed-shape fields become constant-offset accesses; host properties become
indexed getter/setter calls. They must not be mistaken for raw memory. [D5]

Use local constant propagation, deferred literal materialization, destination
passing for expressions, immediate operands, and dead temporary reuse as
initial candidates. Do not move faults, user calls, allocations, or reads of
shared mutable state across observable effects. Preserve the required order
of labeled arguments even when that introduces staging slots/moves. [D5]

Do not clone code for every possible type/constant combination. Existing
generic instantiation limits remain, and generated instruction variants get
a separate build-size budget. Whole-program optimization and speculative
inlining do not belong in the first source-to-result path. [D5, D13]

## Reducing emitted work and memory traffic

Each item below is an experiment with a stated measurement; none is assumed
to pay. They are ordered by expected value per unit of complexity, which is
also the implementation order.

**1. Lazy value materialization.** Lowering describes each expression as a
constant, an existing slot, or an operation still waiting for its
destination, and allocates storage or emits a move only when a consumer
needs one. Constant propagation, copy elimination, immediate selection, and
destination passing then become one mechanism instead of four passes. Lua's
code generator describes expressions the same way (`VKINT`, `VNONRELOC`,
`VRELOC`: "expression can put result in any register"), and V8's Liftoff
records a constant "in the virtual stack" and emits it only when a later
operation uses it ([R28](vm-research.md#r28-deferred-materialization)). It
must not move faults, calls, allocations, or shared reads across observable
effects. [D37, E3]

**2. A slot allocator built for short operands.** Dead temporaries are
reused at once, in stack order as Lua's free-register counter does, so most
temporaries stay in the low slots. Parameters and values used inside loops
are placed below 256 first, so `WIDE` stays rare. The allocator remains a
single linear pass; there is no graph coloring in the source-to-result path.
E3 reports `WIDE` frequency, executed moves, frame size, and compile time
together. [D38, E3]

**3. Temporaries in CPU registers.** Every operation now loads its inputs
from frame slots and stores its result back. A generated interpreter can
carry one integer and one float temporary in host registers between
handlers, spilling at calls, safepoints, and control-flow joins, as wasm3
does with `r0` and `fp0`. The cost is variants: wasm3 reports up to 3
operations per opcode with one cached register and 10 with two
([R19](vm-research.md#r19-hand-written-and-register-cached-interpreters)),
and every variant spends [opcode space](bytecode.md#opcode-manifest). The
first experiment caches scalars only, so stack maps are unchanged, and only
for the families the corpus shows are hottest. Inspect the assembly for
spills before timing it. [D39, E12]

**4. Pre-sized literals.** Array literal lengths, map entry counts, and
struct layouts are known at compile time and are passed to the allocation
helper, so building a configuration never grows a collection
incrementally. Lua's table constructor counts array and record entries
(`na`, `nh`) and passes them to `OP_NEWTABLE`
([R29](vm-research.md#r29-sized-constructors)). Each evaluation still creates
a new, independent object. [D40, E14]

**5. Metadata footprint.** Four-byte instructions can travel with large call
descriptors, root maps, and site tables. Separate what the call handler reads
(target, argument and result spans) from what only the collector, debugger,
and fault path read (root maps, signatures, source spans); share identical
root maps within a module; and report **total executable-artifact bytes**,
not code bytes. The current `CallSite` record stays the correctness
baseline. [D41, E13]

**6. Fusion chosen by measurement.** Count instruction pairs and short
sequences in the reference corpus, statically and as executed, and generate
only candidates that remove a meaningful share of dispatches or slot
traffic, with compile time and handler bytes in the comparison. vmgen's
rules apply: an instruction that changes the PC is only the last component,
and its authors advise against investing much in static superinstructions
([R30](vm-research.md#r30-generated-superinstructions)). A fused instruction
also may not contain a safepoint or a host call. [D10, D42, E5]

## Frame and call model

Start on 64-bit targets with eight-byte slots, untagged typed scalars, and a
pointer-preserving slot representation. In Rust, slot manipulation must
preserve pointer provenance; an arbitrary integer cast is not a tracing or
aliasing proof. Multiword values occupy slot ranges. Heap arrays keep their
element storage layout, including compact `f32` and C-layout extern structs;
VM slot width does not dictate heap element stride. [D7]

Keep control frames separate from value slots. A control frame records the
function/code owner, frame-base offset, continuation PC, result destination,
root-state ID, closure environment, and cleanup state where present. Fields
may be omitted or shared in a later compact representation. No fixed header
byte count is promised. Base offsets survive coroutine-stack relocation.

First implement non-overlapping frames with explicit argument/result copies.
Arguments are evaluated in source order into a contiguous caller window.
The call instruction identifies a checked call site, whose descriptor gives
target, slot spans, and result destination. Host callbacks read that window
directly. Script calls initially copy into a callee's precomputed frame.
Aggregate copying scales with the actual value size. [D8]

Then compare shared tail windows: callee arguments/results overlap a caller
outgoing area only when no caller-live value can be overwritten. Nested
argument calls need separate staging; partially prepared arguments cannot
share a destructively reused window. Caller maps describe pre-call and
suspended state, and the callee has its own map. Adopt overlap only if E4
wins enough to justify the extra layout/rooting logic.

Script recursion uses the VM frame stack rather than unbounded Rust handler
recursion. Suspension returns a control status to the host and keeps VM
frames; no native host stack is captured. The first native prototype can
return to a VM trampoline at script calls, returns, and suspension. Direct
native call chains are a later ABI experiment, not a hidden dependency of
the baseline backend. [D8, D14]

## Interpreter and the CPU

Start with a portable Rust `match` decoder as the correctness/performance
baseline. Inspect release assembly: source-level dispatch syntax does not
establish branch behavior, register pinning, or tail-call elimination. Test
generated continuation/tail-call handlers with an appropriate backend if
the portable compiler cannot produce the intended shape. [D9]

Keep current code, PC, frame base, and runtime state in hot local state where
possible. Publish authoritative state before GC, host calls/reentry,
suspension, and faults. Invalidate/reload local pointers after stack growth
or reentry. Outline cold helpers and keep the common opcode path short.

CPU-inspired does not mean implementing an out-of-order CPU in software.
Our useful analogue is a small typed software ISA in the AArch64 style:
regular families with fields, fixed field positions, load/store access to
memory, compares fused into branches, and no flags
([design discipline](bytecode.md#design-discipline), D28–D35). It is not
hardware microcode.
Fewer VM instructions can remove dispatch and dependent PC updates, but
larger fused handlers can harm code locality. [D9, D10]

What the VM *does* take from microarchitecture is the cost model of the core
its handlers run on. Each row below is a behavior measured on particular
cores, with the VM hypothesis it suggests. None is guaranteed on every
target: R7 itself reports substantial variation across cores and workloads.
Each hypothesis is checked in the generated assembly and timed before it
becomes a rule.

| Observed on the cores measured | VM hypothesis | Evidence |
|---|---|---|
| On the Intel and AMD cores measured, an adjacent compare and conditional jump fuse into one µop; since Ice Lake not when the compare has a memory operand | Compare-and-branch never writes a boolean slot; its handler loads both operands into registers before comparing, so the host pair can fuse; confirm in the assembly. Fold the budget poll into the backward form | [M1](vm-research.md#m1-macro-op-fusion), D19 |
| On the x86 cores measured, hot code runs from a µop cache of roughly 2–6 thousand µops, and an interpreter with many handlers does not fit the loop buffer | Hot handlers have a size budget; overflow, bounds, GC, and host paths are out of line | [M2](vm-research.md#m2-µop-cache-and-loop-buffer), D20 |
| ITTAGE-class indirect predictors predicted most dispatches in the interpreters measured | Start with plain `match` dispatch and measure mispredictions before building threaded variants | [M3](vm-research.md#m3-indirect-branch-prediction), [R16](vm-research.md#r16-pessimistic-modern-predictors-erase-most-dispatch-cost), D9 |
| Many cores eliminate register-to-register moves at rename; memory moves are never eliminated | Copy coalescing keeps executed slot moves rare | [M4](vm-research.md#m4-register-renaming-and-move-elimination), D22 |
| On the cores measured, a load forwarded from a store costs 2–5 cycles, and a wider or misaligned read can cost 12–28 | Store slots at full width; never read a slot wider than written; copy aggregates slot by slot | [M5](vm-research.md#m5-store-to-load-forwarding), D21 |
| Value prediction speculates and recovers | Cached sites use per-runtime side tables; in-place quickening of a private code copy is the experiment | [M6](vm-research.md#m6-value-prediction-and-its-software-analogs), D23 |
| On the cores Agner measured, a branch predicted better than 75% beats a conditional move on a dependency chain | Checks are never-taken branches to cold code; the next PC is chosen by a real branch | [M7](vm-research.md#m7-branches-versus-conditional-moves) |

These rules are tested in E3 and E5 on both x86-64 and AArch64, on the
smallest µop-cache core targeted, because Intel and AMD differ by several
cycles on exactly these paths.

Initially test compare-and-branch and arithmetic-immediate combinations.
Every fused operation retains the original fault PC, polling points, source
evaluation order, and root state. A host call, suspension, or unresolved
aliasing boundary ends a fusion region unless the combined operation
explicitly models it. No giant hand-picked catalogue is assumed. [D10]

## Dynamic sites

Static code has no inline-cache guard. A dynamic site uses a bounded cache
of resolved access recipes; its miss path performs the full operation.
Start with one cached recipe and test a small polymorphic alternative. [D11]

For a fixed class shape, a recipe can guard exact runtime type and binding
version, then load an offset or invoke an accessor. Include lexical visibility
in the site descriptor. For maps, type alone cannot justify an entry offset:
two `map[string]dyn` objects may have different insertion orders. Cache map
identity plus structural version, or use a shared-shape scheme with a real
proof. A property recipe invokes the getter; it never caches away its effects.

Caches containing GC references participate in tracing or use checked weak
references. They cannot keep raw dangling pointers after code/object reclamation.
On a failed guard, fall back before observable effects; an arithmetic native
tier with no speculative representation changes needs no general deoptimizer.

## Roots, faults, and budgets across tiers

Retain precise safepoints and the first non-moving collector. Root metadata
needs both ordinary reference slots and **dynamic root recipes** identifying
`dyn` type/payload slot pairs, including pairs nested in aggregates. A plain
reference bitmap alone does not encode these. Heap descriptors need the
same distinction. Split unions initialize inactive reference positions. [D12]

At allocation safepoints, publish the pre-instruction map before allocating;
temporaries in helpers are explicit roots until stored in a frame. Call maps
exclude uninitialized return slots and describe the suspended caller.
Native code spills live references to canonical VM slots at safepoints in
the first backend. Track precise numeric fault PCs separately from the last
GC safepoint; they need not be the same program point.

Budget polls remain on calls and taken backward edges in every tier, including
fused handlers. Preserve their defined debits when changing instruction form;
do not silently make native code unmetered. Cleanup uses the existing separate
budget/reserve. Host work can block outside VM control; no backend can promise
a hard time limit on arbitrary synchronous host code.

## JIT principles without a JIT

Eitri applies the principles that make JIT compilers fast, but at compile
time or at first use, never through warmup. [Warmup is unreliable even in
mature VMs](vm-research.md#r21-pessimistic-warmup-is-unreliable): at most
43.5% of measured (VM, benchmark) pairs consistently reached peak steady
state.

| JIT principle | In a JIT | In Eitri | Evidence |
|---|---|---|---|
| Specialize on types | From type feedback, after warming up | From the static types, before the first instruction runs | [R20](vm-research.md#r20-interpreters-win-short-runs-untagged-values-pay), [R22](vm-research.md#r22-quickening-and-inline-caches) |
| Compile lazily | Compile a function when it gets hot | Check everything up front; lower a function to bytecode at its first call | D26, E10 |
| Inline caches | At every dynamic send | Only at `dyn`, host-value, and constant-key map sites | [R10](vm-research.md#r10-cache-dynamic-lookup-with-complete-guards), D11, D23 |
| Version code by facts | Basic block versioning | Static types already decide; capped lazy versioning only if `dyn`-heavy code needs it | [R23](vm-research.md#r23-lazy-basic-block-versioning) |
| Tier up | Interpreter, then baseline, then optimizing | The interpreter is the default; a baseline native tier must beat it on measured break-even, including first-use code costs | [R24](vm-research.md#r24-pessimistic-a-copy-and-patch-jit-that-rarely-beats-its-interpreter), D14, D25 |
| Deoptimize | Fall back when a speculation fails | No speculation on static code, so no deoptimizer; a failed cache guard just takes the slow path | D11 |
| Hot/cold layout | Separate fast paths from slow paths | Handlers keep only the fast path; everything else is out of line | [M2](vm-research.md#m2-µop-cache-and-loop-buffer), D20 |

**Lazy lowering** (D26) matters most for setup scripts: in the
[reference config](../examples/editor-config/), most functions are callbacks
(keymaps, autocommands, the statusline) that never run during setup. Checking
stays complete before anything runs, so no type error can surface late; only
bytecode emission moves to the first call. V8's lazy compilation is the
precedent, along with its known cost: work done once to check and again to
lower. Skipping emission does not skip retention: each unlowered function
keeps its checked body, capture layouts, and the metadata lowering needs,
and these must survive the reset of the compiler's scratch arenas, so they
live in a retained arena that E10 counts. The first call of each callback
pays its lowering inside a host call, and E10 reports that latency
separately. Lazy lowering stays an experiment until it beats eager emission
on both total time and retained memory.

## Build-time work, JIT, and AOT

Define operation semantics, operands, effects, fault modes, and safepoints in
one machine-readable specification. Generate opcode declarations, encoders,
decoders, disassembly, metadata tables, and verification scaffolding first.
Generate interpreter/native implementations only where the chosen toolchain
can preserve that same semantics; shared names alone do not prove equivalence.
Differential execution remains necessary. [D13]

Build optimized handlers, cold helpers, optional native stencils, and typed
host descriptors when the runtime/host is built. That is useful AOT work even
when submitted scripts remain fresh source. [D16]

Compare a small single-pass emitter with copy-and-patch for baseline native
code. Neither requires a heavyweight optimizer in the runtime. The host may
request native compilation of known hot callbacks; automatic tiering can use
bounded entry/backedge counters only after their overhead is measured. First
support promotion at later function entries; OSR for already-running loops
is a distinct experiment. [D14, D15]

Optional script AOT can specialize the same typed operations, or investigate
interpreter partial evaluation. An artifact identifies target/ABI, semantic
version, type layouts, host signatures, and required features. Loading links
runtime IDs and constants rather than restoring stale raw addresses. Untrusted
bytecode requires verification. AOT and source submission remain separate
reported workloads; precompilation cannot satisfy the fresh-text target. [D17]

## Implementation order

1. Build the measured scalar/branch/call subset and compact bytecode described
   in [Bytecode](bytecode.md), with a disassembler and portable interpreter.
2. Add root metadata, a minimal collector, retained host callbacks, and fault
   cleanup before comparing embedding performance.
3. Run E1–E7; keep only justified layout, dispatch, and fusion choices.
   Within the compiler, add work reductions in this order: lazy
   materialization, cheap slot reuse, pre-sized literals, then measured
   register caching and fusion
   ([Reducing emitted work](#reducing-emitted-work-and-memory-traffic)).
4. Prototype one baseline native backend on the same subset and run E8 with
   first-use code-memory costs included.
5. Explore offline specialization and broader operation generation through E9.

This order is an engineering plan, not research evidence that a bytecode-first
implementation is fastest. E1 is allowed to change the default execution path.
