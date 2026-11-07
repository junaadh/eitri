# Overview

## What Eitri is for

Eitri, named after the smith of Norse mythology, is a small, typed, embeddable
scripting language built for **immediate execution**. The host hands Eitri a
source string or an `init.eit` file, and Eitri compiles and runs it. It needs
no build step, precompiled cache, or JIT warmup.

The target is **sub-microsecond to a few microseconds** from source text to
execution for one small module in an initialized runtime, and a whole
editor-sized configuration (a [respectable Neovim config](examples/editor-config/),
about 600 lines) well under a millisecond, measured against equivalent Lua
programs. File access, fresh runtime creation, and host
work are measured separately. See [Performance goals](implementation/performance.md).

## The founding principle

> **Resolve what is known, specialize what is learned, execute directly, and
> make every recurring cost earn its place.**

The language, compiler, instruction set, value representations, and VM are
designed together, borrowing the discipline of processor design. Each layer is
treated like CPU microcode: precise operations, explicit data movement,
predictable layouts, and a concrete execution cost for every feature. Anything
that doesn't pay for itself is removed.

JIT thinking is built into the compiler and VM from the start, not added as a
later tier:

- **Resolve what is known.** Names, types, field offsets, call targets,
  argument labels, and defaults are resolved at compile time. A script is
  fully checked before its code runs.
- **Specialize what is learned.** Code is specialized immediately from known
  types. Stable bindings are resolved once. The explicit dynamic values use
  guarded inline caches.
- **Execute directly.** One-shot scripts avoid expensive profiling. Most
  operations compile to one specialized instruction.
- **Make every recurring cost earn its place.** Quickening and fused
  instructions are added only where measurements justify their overhead. A
  native tier is optional, and if added, it can't change semantics.

## What the principle implies

1. **Every feature maps to static dispatch or a known instruction sequence.**
   There are no hidden dynamic lookups.
2. **A script is fully checked before any of its code runs.** Type errors are
   never discovered halfway through execution. Rules that would otherwise be
   runtime checks, such as "no suspension across host frames", are made into
   type rules where possible.
3. **Compilation is part of the cost.** The operation being optimized is
   source text to result, so work moved to compile time is not free. The
   pipeline is short, with local inference, reusable scratch memory, and
   lightweight local optimizations. Module-wide checks must have a stated
   algorithm and cost bound, and nothing looks across modules.
4. **Data has deliberate representations.** Arrays, maps, and fixed-shape
   records are distinct. Typed scalars carry no per-value tag. The
   scalars are `i8`, `i64`, `u8`, `u64`, `f32`, `f64`, and `bool`, with every
   width in the name and none depending on the target, so a script means the
   same thing on every host. Optionals,
   unions, and dynamic values have representations chosen on purpose.
5. **Types describe meaning, not storage.** The spec promises observable states
   and behavior, never sizes or layouts. Narrow values don't imply narrow VM
   slots: slot width is chosen by benchmark, since packing slots can add
   decoding costs.
6. **Failures have exactly two shapes.** A recoverable *error* is a value of
   type `E!T`. An unrecoverable *fault* aborts the current host call. There is
   no throwing.
7. **The host is in control.** The host decides which modules can be imported,
   how much memory and time a script gets, which host objects it can see, and
   when suspended coroutines resume.

## Scripting convenience

The discipline doesn't come at the expense of convenience. Eitri has local
type inference, concise functions, default and named arguments, lambdas,
closures, destructuring, interpolation, optional chaining, null coalescing,
tagged unions, exhaustive pattern matching, explicit modules,
[coroutines](design/coroutines.md), and deferred cleanup. Variables are local
by default, and dynamic values are explicit.

## Lua parity

Eitri is a modern redefinition of Lua. **Every outcome a Lua script can
achieve must be achievable in Eitri**, though the mechanism may differ. Where
Lua resolves names and shapes at run time through tables, Eitri usually
reaches the same result through types, with a known cost. The full map,
including the few remaining gaps, is in [Eitri and Lua](lua.md).

## Influences

| From | Eitri takes |
|---|---|
| Swift | The overall feel, value semantics for `struct`, `let`/`var`, `guard`, `if let`, `mutating`, enum shorthand (`.Dark`), `try await` |
| Rust | Errors as values (`E!T`), exhaustive `match`, pattern rigor, no implicit truthiness, trapping overflow |
| TypeScript | Expression conveniences: `?.`, `??`, arrow lambdas, `${}` interpolation |
| Lua | Its role and every capability: the embedding model, stackful coroutines, closures, cheap startup. See [Eitri and Lua](lua.md) |

## A taste

```eit
import app from "app"

enum Theme {
    Light
    Dark
    Custom(color: string)
}

let theme: Theme = if app.dark_mode { .Dark } else { .Light }

match theme {
    .Light => app.set_background("#ffffff"),
    .Dark => app.set_background("#111111"),
    .Custom(color) => app.set_background(color),
}

app.on_start(() => {
    await app.sleep(0.5)
    app.log("ready")
})
```

For a longer walk-through, see [A tour of Eitri](tour.md).

## What is not here yet

The language design is complete for this draft. What remains open is listed in
[Open questions](open-questions.md): implementation decisions that need a
working VM and measurements, and the features staged for later releases.
