# Virtual machine

Eitri runs on a register-based bytecode VM.

The detailed next-step proposal is in [Execution design](execution-design.md),
[Typed bytecode](bytecode.md), and the [research decision ledger](vm-research.md).
It treats the current register VM as the default candidate, not a measured
winner over checked-tree execution or immediate native compilation. Encoding,
slot, dispatch, and tiering choices remain experiments until implemented.

## Registers and frames

- Registers are **frame slots**, not CPU registers.
- Each function's frame size is computed at compile time.
- Temporary slots are reused within a frame.
- Slot width is chosen **by benchmark**, not assumed up front.

## Instructions

- Instructions are **32-bit words**. Operands that don't fit use extension
  words that follow the instruction.
- Immediate operands avoid constant-pool loads for small values.
- The instruction set is small and regular, in the AArch64 style: 25
  families (arithmetic, conversion, compare-and-branch, load/store, calls)
  whose condition, signedness, overflow mode, and width are fields. A field
  that would branch inside a handler selects a generated handler instead.
- Arithmetic touches frame slots only; fields, elements, and module
  bindings are reached through loads and stores.
- Integer slots hold values extended to 64 bits, so narrow integer
  arithmetic is a 64-bit operation followed by one narrowing.

> ⚠️ **Proposed:** [Typed bytecode](bytecode.md) specifies candidate encodings
> and a first executable subset. The complete opcode set and the performance
> of those encodings remain open until the first VM is measured.

## Values

- **Typed scalars carry no per-value tag.** The compiler knows every slot's
  type, so an `i64` in a slot is just the bits.
- **Host objects are opaque handles.**
- Representation of optionals, results, and enums is up to the
  implementation; see [Types](../language/types.md#meaning-not-storage).

## Garbage collection

T015-1 adds non-moving string objects with headers and mark-sweep collection.
Pointer-preserving references occupy a sidecar to the scalar `u64` stack;
verified ordinary root maps select references at allocations, calls, and taken
backward branches. Root construction for managed functions currently supports
only straight-line code; dynamic roots, aggregates, handles, and managed
signatures remain later work. Forced-collection tests run both interpreters.
The managed-object byte limit counts object structures and string payloads;
allocator overhead, collection scratch, compiler storage and code are not yet
covered. Collection is expected O(objects + roots), with hash-collision
worst cases quadratic; reference lookup is expected O(1).

Classes, growable arrays, maps, and strings may be heap allocated and
garbage collected.

Because scalars are untagged, the collector can't scan frames blindly. It
needs to know, at the moment it runs, which slots hold references. A single
reference/scalar boundary per frame isn't enough: structs can mix references
and scalars, enum payloads differ by case, and temporary slots are reused for
values of different types.

**Planned frame model.** This is the first VM experiment, because it affects
frame size, compile time, and suspended coroutines.

- **Safepoints.** The collector runs only at safepoints: calls, allocations,
  budget checks at backward branches, and suspension points.
- **Stack maps.** For each safepoint, the compiler records a bitmap of the
  frame slots that hold live references there. The register allocator
  already knows each slot's type and liveness, so reused temporaries are
  handled: the map describes the slot at that point in the code, not for the
  whole function.
- **Flattened structs.** A struct in a frame occupies several slots, each
  statically a reference or a scalar, so the stack map covers it.
- **Split unions.** Enums with payloads, and `E!T`, lay out their cases so
  that ordinary reference positions never hold scalars. Unused ordinary
  reference slots hold `null`, so tracing those positions needs no tag.
  Dynamic pairs in the remaining partition use the case guards below.
- **Dynamic recipes.** A `dyn` is a type ID plus one word; its type determines
  whether the payload is a reference. In unions the tracer reads the union tag
  first and follows only the active case's dynamic recipes. Inactive pairs are
  unspecified and never read. Construction publishes the tag after initializing
  the active payload, with no safepoint during an in-place transition. See
  [publication details](execution-design.md#roots-faults-and-budgets-across-tiers).
- **Heap objects** carry a pointer to a per-type descriptor listing the
  offsets of their references, generated once per type.
- **Suspended coroutines** are scanned frame by frame, using the stack map at
  each frame's saved program counter.

The experiment measures stack-map size, the extra compile time to produce
the maps, and whether split unions make frames noticeably larger. It also
compares **conservative frame scanning**, which needs no stack maps or split
unions: a study of Java collectors found it falsely retained under 0.01% of
objects and came within 2–3% of exact scanning, though Eitri's untagged
integers may look like pointers more often. See
[R26](vm-research.md#r26-conservative-stack-scanning) and D27.

The detailed proposal additionally distinguishes ordinary reference bitmap
entries from dynamic-root recipes for `dyn` type/payload pairs, including
pairs nested in aggregates. Heap descriptors need both kinds too. See
[Roots across tiers](execution-design.md#roots-faults-and-budgets-across-tiers).

**First collector:** non-moving mark-sweep, which keeps object identity
simple; see [Identity and hashing](identity.md). Generational, incremental,
or moving designs are later experiments.

> ⚠️ **Open:** the collector design beyond the first version, and how it
> interacts with references the host holds.

## Execution strategy

One rule governs execution:

> Resolve what is known, specialize what is learned, and add quickening or
> fused instructions only where measurements justify them.

- **Static first.** Most dispatch is resolved at compile time because the
  language is statically typed.
- **Guarded inline caches** serve the explicit dynamic values (once those are
  designed).
- **Quickening and fused instructions** are added only when benchmarks show a
  win. The evidence on modern CPUs runs both ways: branch predictors now make
  plain dispatch cheap, yet fusion still saves slot traffic and dispatches.
  See [the CPU cost model](execution-design.md#interpreter-and-the-cpu) and
  [R16–R18](vm-research.md#r16-pessimistic-modern-predictors-erase-most-dispatch-cost).
- **A native tier is optional.** If one is added, it must not change
  semantics.

## Identity and hashing

Equality and hashing keep fixed invariants; the first implementation uses
addresses for objects and cached content hashes for strings. See
[Identity and hashing](identity.md).

## Closures

A captured variable that is assigned after capture becomes a **shared
variable**; everything else is copied into the closure (see
[Captures](../language/bindings-and-functions.md#captures)). Two
implementations are possible, to be chosen by benchmark:

- **Heap cell at declaration.** The variable lives in a heap cell from the
  start, and every access, including the declaring function's own, goes
  through it. Simple, with one indirection per access.
- **Open upvalues, as in Lua.** The variable stays in its frame slot while
  the frame is alive, so the declaring function accesses it directly. When
  its scope ends, it moves to the heap and closures switch to the heap copy.
  Coroutine stacks can move when they grow, so open upvalues must be found
  and fixed up on a move, or refer to slots by frame-relative offset.

The semantics don't depend on the choice.

## Coroutines

Coroutines are stackful. Each one owns a segment of VM frame stack, and an
`await` call is an ordinary call on that stack. Suspending keeps every script
frame in place and hands control back to the host. Switching coroutines
updates the current coroutine pointer, the stack bounds, GC roots, the pending
`defer` chain, and the host-frame barrier.

A suspended coroutine's frames are GC roots only while the coroutine is
reachable. The GC never runs script code, so an unreachable suspended coroutine
is freed without running its `defer` blocks.

**Requirement:** registers are addressed relative to the frame base, never by
absolute pointer, so a coroutine's stack segment can move when it grows.
Initial segments are sized from the entry function's precomputed frame size.

A task's waiters are resumed in FIFO order. Storing them as an intrusive list
threaded through the waiting coroutines is the planned implementation, not a
semantic requirement.

Switching costs are measured against the goals in
[Performance goals](performance.md#coroutine-costs). See
[Coroutines and async](../design/coroutines.md).

## Budgets

Execution budgets are checked at **backward branches and calls**, so any
unbounded loop or recursion eventually reaches a check. Exhausting the budget
is a [fault](../language/errors.md#faults).

Cleanup after a fault or cancellation runs under a separate cleanup budget,
and may use a reserved block of memory. See
[Cleanup after a fault](../language/errors.md#cleanup-after-a-fault).

## Debug information

Program-counter-to-line tables and other debug metadata live outside the
instruction stream. Fault traces use them to report source locations.
