# Typed bytecode proposal

Status: experimental format, not a stable serialization or public ABI.
Evidence and counterarguments: [D1, D5–D13, D23, D28–D36](vm-research.md#decision-ledger).
See [execution design](execution-design.md) for IDs, frames, GC, and tiering.
Exact widths below are hypotheses for E3 and E11, not values proven by a paper.

## Design discipline

The instruction set follows the AArch64 discipline: a small set of regular
instruction families with fields, rather than one opcode for every
combination of operation, type, and operand kind. The specification stays
small. The handlers are generated from it.

1. **Families with fields.** An instruction is a family (`ADD`, `CB`, `LD`)
   plus fields: condition, signedness, overflow mode, width, operand kind.
   The first subset is the 25 families in the
   [table below](#instruction-families). [D28]
2. **Dispatch fields and operand fields.** A hardware decoder extracts fields
   in parallel as part of decoding; an interpreter spends instructions on
   every field it extracts, and a branch on every field that selects
   behavior. So a field that would choose a different code path inside the
   handler is a *dispatch field*: it is part of the opcode byte, and each
   value gets its own handler, generated from one template. The condition of
   a compare-and-branch, wrap versus checked, access width, and register
   versus immediate are dispatch fields. A field that only feeds arithmetic
   is an *operand field*, decoded inside one handler without branching: an
   extension width, a scaled offset. Combinations the compiler never emits
   get no opcode. [D28]
3. **Fixed field positions.** In every short format the opcode is bits 0–7
   and A is bits 8–15; B and C, where present, are bits 16–23 and 24–31.
   The dispatch sequence can extract the opcode and A before the indirect
   jump, as LuaJIT's `ins_NEXT` does, so no handler decodes them again.
   Whether a given host compiler does this is checked in its output. [D29]
4. **Load/store.** Arithmetic reads and writes frame slots only. Memory
   outside the frame (fields, elements, module bindings, captured
   variables) is reached only through `LD`, `ST`, and calls. [D34]
5. **Signedness only where the bits differ.** Wrapping add, subtract, and
   multiply, equality, and the bitwise operations give the same bits for
   `i64` and `u64`, so one handler serves both, as AArch64's `ADD` does.
   Signedness is a field only for checked overflow, ordering, division,
   right shift, and conversion. [D30]
6. **Narrow integers compute at 64 bits, then narrow.** Every integer slot
   holds its value sign- or zero-extended to 64 bits (the *canonical form*,
   which extends D21). Arithmetic on `i8` through `u32` is the 64-bit
   operation followed by one `CVT` that narrows, checked or wrapping.
   AArch64 also has no byte or halfword arithmetic: it uses full-register
   operations and the `UXTB`/`SXTB` extends, which are aliases of one
   bitfield-move instruction. **Shifts are the exception:** the language
   faults when the amount is outside `0..bits` of the operand's type, so a
   shift handler needs the source width. Shifts carry width as a dispatch
   field and narrow inside the handler; see
   [narrow arithmetic](#narrow-arithmetic). [D31]
7. **No flags.** A compare is fused into the instruction that consumes it: a
   branch (`CB`, `FCB`) or a boolean write (`CSET`, `FCSET`). An
   interpreter's flags register would be a slot written by one dispatch and
   read by the next, which on the cores in M5 is a store-to-load round trip
   across an indirect jump. RISC-V made the same choice for its hardware
   branches, and Armv9.6 added register-register compare-and-branch. [D32]
8. **Select, not predication.** No instruction has a guard field. AArch64
   dropped ARM32's conditional execution of every instruction and kept
   conditional select. Eitri's `SEL` is a candidate, admitted only by
   measurement. [D33]

What is deliberately not taken from AArch64:
- **A flags register (NZCV) and conditional compare (`CCMP`)**, for the
  reason in rule 7.
- **A zero register.** It would reserve a slot, and all 256 short slots stay
  usable.
- **Bitmask logical immediates, and `MOVZ`/`MOVK` chains.** A software decode
  of a bitmask costs more than a constant-pool load, and a four-instruction
  constant is four dispatches against one `LDK`.
- **`MADD`.** Its four operands do not fit one word.

## Executable function

Each function owns immutable code words, its constants and site descriptors,
frame/parameter/result layouts, root-state maps, and PC-to-source information.
The constant pool owns references needed by code. Published code is
executable only after the whole submitted unit has passed checking and
linking.

**Code is immutable. Mutable execution state lives in side tables.** Code
can be shared between runtimes created from one
[runtime template](embedding.md), and the runtime that executes it never
writes it. Each runtime owns, per function, a side table with tier counters
and one entry per cached site; a site's descriptor names its entry index.
No handler rewrites an instruction. In-place quickening, which needs a
per-runtime mutable copy of the code, is an experiment with its rules stated
in [Caches and quickening](#caches-and-quickening). [D23]

Use word-index PCs. No instruction may branch into an operand word. The
runtime code buffer is native `u32` words; an eventual serialized format
specifies byte order and version separately. Its words contain indexes and
immediates, not executable host addresses.

## Compact encodings

Bit 0 is the least significant bit:

```
ABC:   [ C:u8       | B:u8       | A:u8 | opcode:u8 ]
ABx:   [ X:u16                  | A:u8 | opcode:u8 ]
AsBx:  [ X:i16                  | A:u8 | opcode:u8 ]
Ax:    [ X:u24                         | opcode:u8 ]
sAx:   [ X:i24                         | opcode:u8 ]
```

The opcode byte is the handler index: the family plus its dispatch fields.
Slots have no reserved sentinel; all 256 short slot indexes are usable.
Register and immediate forms are separate opcodes rather than a flag tested
on every operand (Lua 5.4 tests a `k` bit instead). That trade-off is
measured against the extra handler footprint.

Operands that do not fit use one of three mechanisms, chosen so that the
opcode table does not double:

```
WIDE:    word 0 = [ C_hi:u8 | B_hi:u8 | A_hi:u8 | WIDE ]
         word 1 = the instruction, whose A, B, C are the low bytes

J_X:     word 0 = [ reserved:u24 | opcode:u8 ]
         word 1 = displacement:i32

LDK_X:   word 0 = [ reserved:u16 | A:u8 | opcode:u8 ]
         word 1 = constant index:u32

LDF_X, STF_X:
         word 0 = [ B:u8 | width and signedness:u8 | A:u8 | opcode:u8 ]
         word 1 = byte offset:u32
```

- **Slots above 255** use the `WIDE` prefix, which supplies the high byte of
  each slot operand, as the JVM's `wide` does. The experimental frame limit
  is 65,536 slots; larger functions produce a resource diagnostic. The rules
  are in [The WIDE prefix](#the-wide-prefix).
- **Far branches.** A conditional branch has a short displacement only. A
  conditional branch to a far target is the inverted short branch over a
  `J_X`, as assemblers do for an out-of-range AArch64 `B.cond`. The far path
  costs one more dispatch; no conditional family needs a long form. This
  needs every condition's exact complement, including the unordered float
  conditions, which the [condition set](#instruction-families) provides.
- **Large constants and offsets** use `LDK_X`, `LDF_X`, and `STF_X`, one
  dispatch each.

Reserved bits are zero. Code/pool size limits are checked before narrowing
indexes, and allocation arithmetic is checked for overflow.

### The WIDE prefix

`WIDE` is not a mode. It is the first word of one instruction whose length
is 1 + the follower's length: two words before a short form, three before
`LDK_X`, `LDF_X`, or `STF_X`. No prefix state exists outside that
instruction:

- **One instruction.** The `WIDE` handler reads the following word, combines
  each high byte with the matching low byte, and enters the follower's
  handler with the widened operands. That is a second indirect jump, paid
  only in frames larger than 256 slots. PC advances by the whole length at
  once;
  a fault, safepoint, or suspension inside it reports the `WIDE` word's PC.
  The instruction-start map marks only the `WIDE` word, so no branch can
  target the follower.
- **Legal followers.** Any short-form instruction (and `LDK_X`, `LDF_X`,
  `STF_X`) whose specification marks at least one of A, B, C as a slot
  field. A high byte may be nonzero only for a slot field; the high bytes of
  immediates, displacements, and operand fields must be zero, so `WIDE`
  never widens a displacement or a constant. In `LD`/`ST`, C is a slot only
  in index mode.
- **Illegal.** `WIDE` followed by `WIDE`, `J`, `J_X`, `CALL`, or `RET`;
  `WIDE` as the last word of a function; a widened slot outside the frame.
  The verifier rejects each of these, and the decoder never sees them in
  verified code.

All branch displacements are signed two's-complement **word offsets from
the end of the entire instruction** (including a `WIDE` prefix). Assembler
output can show labels; encoded code contains displacements. The
instruction-start map is metadata, not an opcode in the execution stream.
A `WIDE` operand word cannot be a branch target.

## Branch emission without repeated relaxation

Changing short branches to far branches changes subsequent PCs. Do not run
an unbounded widen/recompute loop during a latency-sensitive compilation.
First implementation: emit backward branches short when the known target
fits; for forward branches, compute a conservative upper bound of the
distance from the logical instructions between branch and target, and emit
the far form only when that bound does not fit. Labels, roots, and debug
spans use logical instruction IDs until final word PCs are assigned. [D6, E3]

## Instruction families

These are the families and their fields. Suffix `_X` means the
corresponding form above. Every taken backward branch (`CB`, `CBI`, `FCB`,
`TB`, `J`, `LOOP`) polls the budget.

| Family | Format | Dispatch fields | Operand fields | Semantics |
|---|---|---|---|---|
| `MOV` | ABC, C=0 | | | A ← B, preserving pointer representation |
| `MOV2` | ABC | | | A ← B and A+1 ← C: a pair move, like `STP` |
| `LDI` | AsBx | | | A ← sign-extended X |
| `LDK` | ABx | | | A ← checked one-slot constant X |
| `ALU` | ABC | operation; wrap, checked signed, or checked unsigned; signedness for `DIV`, `REM`; width for shifts | | A ← B op C at 64 bits. Checked overflow, division by zero, and shift amounts outside the width fault at this instruction |
| `ALUI` | ABC | as `ALU`, only for the operations in the manifest | C: i8 immediate, or u8 shift amount | A ← B op C |
| `UN` | ABC, C=0 | `NEG` wrap, signed checked, or unsigned checked; `NOT` | | A ← op B |
| `CVT` | ABC | wrap or checked | C: destination width (8, 16, 32, 64), destination signedness, source signedness | Integer conversion to canonical form; checked faults when the value does not fit |
| `FALU` | ABC | operation; `f32` or `f64` | | IEEE operations; no reassociation or fast-math |
| `FCVT` | ABC | source and destination kind | C: integer width and signedness | Integer↔float and `f32`↔`f64`, with the language's rounding and range rules |
| `CB` | ABC | integer condition | C: i8 displacement | Branch if A cond B |
| `CBI` | ABC | integer condition | B: i8 immediate, C: i8 displacement | Branch if A cond B (the immediate) |
| `FCB` | ABC | float condition; `f32` or `f64` | C: i8 displacement | Branch if A cond B, with IEEE NaN behavior |
| `TB` | AsBx | true, false, none, or some | X: displacement | Test a bool or an optional's tag, and branch |
| `CSET`, `FCSET` | ABC | condition; for `FCSET`, `f32` or `f64` | | A ← (B cond C) as a bool |
| `SEL` | ABC | | | A ← if A { B } else { C }. Candidate, see D33 |
| `J` | sAx | | | Jump by X |
| `LOOP` | AsBx | signed or unsigned; exclusive or inclusive | X: backward displacement (far loops: see [Counted loops](#counted-loops)) | Range loop back edge; see [Counted loops](#counted-loops) |
| `LD` | ABC | addressing mode; access width and extension | C: scaled offset or index slot | A ← memory at B; see [Memory access](#memory-access) |
| `ST` | ABC | addressing mode; access width | C: scaled offset or index slot | Memory at B ← A |
| `CALL` | ABx, A=0 | script, host, or value | X: call-site descriptor | Calls poll the budget |
| `RET` | Ax, X=0 | | | Return the function's declared result span |
| `TRAP` | ABx | | | Fault with the string in A and source/site descriptor X |
| `WIDE` | prefix | | high bytes of A, B, C | Widens the next instruction's slot operands |

**Conditions.** Integer and float conditions are separate families, so each
condition field is 3 bits.

- **Integer:** `EQ NE LT LE LO LS`. `GT`, `GE`, `HI`, `HS` swap the
  operands.
- **Float:** `FEQ FNE FLT FLE FULT FULE`. `FNE`, `FULT`, and `FULE` are also
  true when either operand is NaN (*unordered*); the others are false on NaN.
  Swapping keeps NaN behavior (`a > b` is `b < a`).
- **Immediate:** `CBI` cannot swap, so it has `EQ NE LT GT LO HI`; `LE`,
  `GE`, `LS`, and `HS` adjust the immediate by one at compile time, falling
  back to the register form at the edge of the immediate's range. LLVM does
  the same when it targets Armv9.6's `CB<cc>`, whose condition field is also
  3 bits.

**Every condition has an exact complement in the set**, because the
compiler inverts conditions for every `if` (branching over the body), not
only for far branches:

| Condition | Complement |
|---|---|
| `EQ`, `NE` | `NE`, `EQ` |
| `LT a, b` / `LE a, b` | `LE b, a` / `LT b, a` |
| `LO a, b` / `LS a, b` | `LS b, a` / `LO b, a` |
| `FEQ`, `FNE` | `FNE`, `FEQ` |
| `FLT a, b` / `FLE a, b` | `FULE b, a` / `FULT b, a` |
| `FULT a, b` / `FULE a, b` | `FLE b, a` / `FLT b, a` |
| `TB` true, none | false, some |

Swapping alone is not a complement for floats: on NaN both `a < b` and
`b <= a` are false. The generated manifest checks this table mechanically.

**Float precision.** A float slot holds raw `f32` or `f64` bits (see
[Memory access](#memory-access)), so every float instruction, including
comparisons, has `f32` and `f64` variants. Reading an `f32` slot as `f64`
would, for example, make `-0.0` unequal to `0.0`.

**Aliases.** The disassembler prints the readable name, as AArch64's tools
print `LSL` for a `UBFM`: `CB.GT r1, r2` for a swapped `CB.LT r2, r1`, and
`SXT8 r1` for a `CVT` with width 8, signed, wrapping.

### Narrow arithmetic

For canonical operands, the 64-bit operation followed by `CVT` gives the
narrow operation's result and fault:

| Operation | Lowering for `i8`…`u32` | Why it is exact |
|---|---|---|
| `+`, `-` checked | `ADD.WRAP`/`SUB.WRAP`, then `CVT.CHK` | Operands of 32 bits or fewer cannot overflow 64 bits; the narrowing faults exactly when the narrow result is out of range |
| `*` checked | `MUL.CHK.S`, then `CVT.CHK` | Signed products of 32-bit operands fit 64 bits. A `u32` product can exceed `i64`; the multiply then faults, and that product overflows `u32` too |
| Wrapping `+ - *` | 64-bit wrapping operation, then `CVT.WRAP` | The low bits of the 64-bit result are exact modulo 2^width |
| `/`, `%` | Signed or unsigned 64-bit operation, then `CVT.CHK` | Division by zero faults in the 64-bit operation; `MIN / -1` produces 2^(width−1), which the narrowing rejects |
| Unary `-` | `NEG.CHK.S`, then `CVT.CHK` | Negating an unsigned nonzero value, or a signed minimum, produces a value outside the type |
| `~` | `NOT`, then `CVT.WRAP` | Restores zero extension for unsigned types; already canonical for signed |
| `<<` | `SHL.S.w` or `SHL.U.w`, one instruction | Faults unless the amount is in `0..w`; shifts at 64 bits, then wraps to width `w` and sign- or zero-extends, since bits shifted out are dropped |
| `>>` | `SHR.S.w` or `SHR.U.w`, one instruction | Faults unless the amount is in `0..w`; a canonical value shifted right is already canonical |

Shift width is a dispatch field (`w` is 8, 16, 32, or 64) because the
amount check depends on it and ABC has no free bits. A narrow left shift
also needs signedness: `64 << 1` leaves low bits `0x80`, which is `128` for
`u8` and `-128` for `i8`, from identical input slots. `SHL.64` needs no
signedness, since the 64-bit result is the canonical form for both. `ALUI`
shifts take the amount as an immediate, which the checker validates against
the width, so `SHLI` still wraps and extends but needs no run-time amount
check. Every row is
stated in the operation specification and fuzzed against a reference
implementation of the narrow operation.

### Memory access

A memory access has four separate properties: the access width, the stride
between elements, the extension into the slot, and the destination slot,
which is always a full canonical slot. Reading eight bytes from a one-byte
element is never allowed.

| Element type | Access width | Load extension | Slot contents |
|---|---|---|---|
| `i64`, `u64`, `f64`, references | 8 | none | the bits |
| `i32`, `i16`, `i8` | 4, 2, 1 | sign | canonical |
| `u32`, `u16`, `u8`, `char` | 4, 2, 1 | zero | canonical |
| `bool` | 1 | zero | 0 or 1 |
| `f32` | 4 | zero | the `f32` bits in the low half, upper half zero |

Loads have seven width/extension variants (8; 4, 2, 1 signed; 4, 2, 1
unsigned); stores have four widths and store the low bytes of a canonical
slot, which is exact because the value fits. `LD`/`ST` have four addressing
modes:

- **Field:** base B plus C scaled by the access width. Larger offsets use
  `LDF_X`/`STF_X`.
- **Index:** base B, bounds-checked index slot C, stride equal to the access
  width. Elements larger than one slot (structs in arrays) use the
  aggregate form, which takes a layout descriptor and copies slot by slot.
- **Module binding** and **captured variable:** full slots only.

`@extern` layouts use the same widths; their offsets come from the C layout
rather than from Eitri's own.

### Counted loops

`LOOP` is the back edge of `for i in a..b` and `for i in a..=b` over an
integer type, and it never overflows. The compiler evaluates `a` and `b`
once, into the counter slot A and the limit slot A+1, and emits the entry
test as an ordinary compare-and-branch: skip the loop unless `a < b`
(exclusive) or `a <= b` (inclusive). The body reads A as `i` and never
writes it; a closure that captures `i` captures that iteration's value.

| Form | Back edge | Invariant |
|---|---|---|
| Exclusive `a..b` | A ← A + 1; branch back if A < limit | In the body A < limit, so A + 1 ≤ limit and cannot overflow |
| Inclusive `a..=b` | If A < limit: A ← A + 1 and branch back; otherwise fall through | A is incremented only when below the limit, so it never passes it |

`for i in max..=max`, with `max` the largest `u64`, passes the entry test,
runs the body once, and exits at the back edge without incrementing. The
compare is signed or unsigned by the dispatch field; narrow types stay
canonical because the counter never leaves `a..=b`. A taken back edge polls
the budget. Other strides and reverse iteration lower to `ALU` and `CB`.

**Far loops.** `LOOP` reaches 32,767 words back. A state-changing back edge
cannot be inverted like a plain branch, so a body larger than that uses a
fallback lowering with the same counter update and poll, built from ordinary
instructions (shown signed; unsigned uses `LO`/`LS`):

```
// exclusive a..b                  // inclusive a..=b
ADDI.WRAP  rA, rA, 1               CB.LE     rL, rA, exit   // A >= limit: done
CB.LE      rL, rA, exit            ADDI.WRAP rA, rA, 1
J_X        top                     J_X       top
exit:                              exit:
```

The increments cannot overflow, by the same invariants as `LOOP`, so the
wrapping form is exact. The budget poll is on the backward `J_X`. This costs
three dispatches per iteration instead of one, only for bodies over 32K
words.

### Opcode manifest

The first subset, counted variant by variant. A generated manifest replaces
this hand count once the operation specification exists, and the build
fails if it exceeds 256.

| Family | Opcodes | Variants |
|---|---|---|
| `MOV`, `MOV2`, `LDI`, `LDK` | 4 | |
| `ALU` | 31 | `ADD SUB MUL` × wrap, checked signed, checked unsigned (9); `DIV REM` × signed, unsigned (4); `AND OR XOR` (3); `SHL` × signed, unsigned × 3 narrow widths, plus `SHL.64` (7); `SHR` × signed, unsigned × 4 widths (8) |
| `ALUI` | 16 | `ADD SUB` × wrap, checked signed (4); `ADD` checked unsigned (1); `MUL` checked signed (1); `AND` (1); `SHL` as in `ALU` (7); `SHR` signed, unsigned (2) |
| `UN` | 4 | `NEG` × wrap, checked signed, checked unsigned; `NOT` |
| `CVT` | 2 | wrap, checked |
| `FALU` | 10 | `ADD SUB MUL DIV NEG` × `f32`, `f64` |
| `FCVT` | 6 | integer→`f32`, integer→`f64`, `f32`→integer, `f64`→integer, `f32`→`f64`, `f64`→`f32` |
| `CB`, `CBI`, `FCB` | 24 | 6 + 6 + 6 conditions × `f32`, `f64` |
| `TB` | 4 | true, false, none, some |
| `CSET`, `FCSET` | 18 | 6 + 6 conditions × `f32`, `f64` |
| `SEL` | 1 | candidate |
| `J`, `J_X` | 2 | |
| `LOOP` | 4 | signed, unsigned × exclusive, inclusive |
| `LD` | 17 | field × 7 widths; index × 7 widths; module; captured; aggregate |
| `ST` | 11 | field × 4 widths; index × 4 widths; module; captured; aggregate |
| `LDK_X`, `LDF_X`, `STF_X` | 3 | |
| `CALL` | 3 | script, host, value |
| `RET`, `TRAP`, `WIDE` | 3 | |
| **Total** | **163** | 93 left for maps, optionals and enums, closures, `dyn`, tasks, and quickened forms |

`SHR` immediate forms need no width, because the amount is checked at
compile time and a canonical value shifted right stays canonical. `LDF_X`
and `STF_X` carry width and signedness in their own operand byte, so they
are one opcode each.

For comparison, Lua 5.4 has 83 opcodes, without typed, checked, or unsigned
operations. The architectural specification here is 25 families; the
handler count is what the dispatch-field rule produces, and E11 measures
whether its footprint costs more than the dispatches it saves.

## Caches and quickening

**Side caches (the first implementation).** A cached site (`dyn` access,
host property, constant map key) has an entry in the function's per-runtime
side table. The entry's state moves only forward: empty, then monomorphic,
then polymorphic (at most four recipes), then megamorphic, which is final.
Invalidation (hot reload, a host type redefinition) resets the whole table,
never a single entry backward.

- **Tracing.** Recipes refer to types, bindings, and maps by ID or
  generation, not raw pointer. Where an entry must hold a GC reference, the
  side table is traced as part of its runtime's roots, or the reference is a
  checked weak reference.
- **Reentry.** The slow path can call the host, which can reenter and run
  the same site, advancing its entry. So a fill never stores a recipe
  computed before the call blindly:
  1. Before the slow path, the handler records the table's invalidation
     epoch.
  2. After it returns, the handler re-reads the epoch. If it changed, the
     pending fill is discarded.
  3. Otherwise it re-reads the entry and merges: a megamorphic entry stays
     megamorphic; a recipe already present changes nothing; otherwise the
     recipe is added, which moves empty to monomorphic, monomorphic to
     polymorphic, and a full polymorphic entry to megamorphic.
  4. It stores the merged entry.

  Steps 2–4 contain no call, and a runtime is thread-bound, so nothing runs
  between the re-read and the store. No handler keeps a pointer into the
  side table across a call. The merge is a join, so the state can only move
  forward.

**In-place quickening (an experiment).** Rewriting the opcode itself saves
the guard dispatch on a hit, but code is immutable. So a runtime makes a
private copy of a function the first time one of its sites would quicken,
and rewrites only that copy:

- A rewrite replaces one opcode with another of the same length and operand
  layout; transitions are listed in the specification (generic to quickened,
  quickened back to generic on a failed guard, and then never quickened
  again at that site).
- The executing thread rewrites at an instruction boundary. Since a runtime
  is thread-bound, no other thread observes the copy.
- A suspended frame resumes after its `CALL`, so rewriting that `CALL` while
  suspended is safe only if the result span and root maps are unchanged,
  which the same-layout rule guarantees.
- Equal length keeps PCs, stack maps, and debug spans valid. It does not by
  itself make the rewrite correct, which is why the transitions are
  enumerated.

E6 measures the side table against quickening, including the memory and
copy cost of the private copies.

## Call descriptors

The initial call site contains:

```
CallSite {
    target: Script(FunctionId) | Host(HostFnId) | Value(slot),
    argument_span: (first_slot, slot_count),
    result_span: (first_slot, slot_count),
    signature: SignatureId,
    pre_call_roots: RootMapId,
    suspended_roots: RootMapId,
    post_call_roots: RootMapId,
}
```

Representation can use separate tables by call kind to avoid reading an enum
tag already implied by the opcode. The descriptor removes counts/layout work
from the handler but adds a table load. Compare immediate target/window forms
in E4; do not assert a descriptor load is free.

Before a script call, arguments are ready in declaration order. The initial
VM allocates the callee frame and copies them; return copies the result span
back. A host callback directly views the caller's argument window. The
active host context and pending result are rooted during reentry. After
reentry or frame-stack growth, handlers fetch base pointers again.

Zero-sized `void` results have an empty span. Results, optionals, and structs
use checked multiword layouts rather than arbitrary tagged-scalar packing.
Pre-call roots never mark uninitialized return storage. A fault leaves the
call site as the caller's trace location and enters cleanup.

## Extending the ISA

New features extend existing families with an addressing mode, a dispatch
field value, or a descriptor kind before they add a family, and every
addition updates the manifest:

| Feature | Information resolved before execution | Runtime work that remains |
|---|---|---|
| Module load/store | Module instance slot and binding slot | Load/store shared state |
| Fixed field load/store | Layout, byte offset, storage width, permissions | Dereference; conversion/check where specified |
| Host property | Registered getter/setter index | Host call and its effects |
| Array load/store | Element layout/stride | Bounds and appropriate address/load/store |
| Maps | Key hash/equality recipe; constant hash where compatible | Lookup, mutation, structural versioning |
| Optional/result/enum | Tag and payload layouts | Branch, active payload transfer, root initialization |
| Closure/upvalue | Capture layout and binding classification | Closure/cell allocation or open-upvalue management |
| `dyn`/interface access | Site descriptor and visible member identity | Guards, lookup fallback, boxing when necessary |
| Tasks/channels/defer | Function effect and cleanup metadata | Allocation, scheduling, cancellation/unwinding |

Large payloads use shared copy/compare/trace helpers. No family is complete
until its read/write spans, layout effects, safepoint behavior, fault modes,
and source-order behavior are defined.

## Worked lowering

```
fn sum(a: i64, b: i64) -> i64 {
    return a + b
}
```

For a frame with result slot 0 and parameter slots 1 and 2:

```
ADD.CHK.S  r0, r1, r2
RET
```

The two words in hexadecimal, using **illustrative** opcode values
`ADD.CHK.S=0x10`, `RET=0x01`, are:

```
0x02010010
0x00000001
```

This example is two dispatches, with no dynamic type tests or name lookups.
It still reads/writes slots and checks overflow. A native backend can keep
values in CPU registers between observability points and encode operand
indexes/constants directly. It must preserve overflow faults and the VM call
contract. [D5, D14]

Narrow arithmetic is the 64-bit operation and one narrowing:

```
fn add(a: u8, b: u8) -> u8 {
    return a + b
}
```

```
ADD.WRAP   r0, r1, r2     // canonical u8 operands: the 64-bit sum cannot overflow
CVT.CHK    r0, r0, u8     // faults exactly when the u8 addition overflows
RET
```

A narrow shift is one instruction, because the width is in the opcode:

```
SHL.U.8    r0, r1, r2     // u8: faults unless r2 is in 0..8; wraps and zero-extends
```

A condition chain needs no booleans. From the
[statusline](../examples/editor-config/statusline.eit):

```
if errors > 0 && warnings > 0 { ... }
```

```
CBI.LE     r1, 0, else    // errors <= 0: skip
CBI.LE     r2, 0, else    // warnings <= 0: skip
...
```

Each condition is one word and one dispatch. Lua 5.4 also uses one dispatch
per condition, because its compare handler performs the following jump
itself, but it spends two words on each: the compare and the jump.

## Verification and shared metadata

The operation specification records each family's encoding, dispatch and
operand fields, operand roles, read/write slot spans, fault/effect classes,
safepoint rules, quickening transitions, and disassembler aliases. Generate
the opcode manifest, handler expansions, decoder, disassembler, and
structural verification from it. Typed checking additionally establishes
slot initialization, canonical integer form, operand types, compatible
control-flow joins, signature/layout correctness, and valid root maps.
Merely checking indexes and opcodes does not make untagged execution safe.

Compiler-produced code can rely on those established invariants rather than
repeat name/type checking in every handler. Untrusted loaded artifacts need
their own complete validation boundary; executable native artifacts require
a separate trust policy. Operand words and `WIDE` payloads cannot be entry
points.

Fuzz encoding/decoding and branch boundaries, compare interpreter/native
outcomes, and force collection at each legal safepoint. No file format or
unchecked execution API becomes stable as a side effect of this proposal.
