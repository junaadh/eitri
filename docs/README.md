# Eitri documentation

Eitri is a small, typed, embeddable scripting language built for immediate
execution. The host hands over a source string or an `init.eit` file, and
Eitri compiles and runs it with no build step, cache, or JIT warmup.

These documents are a **draft specification** (language draft 0.14). Where the
design is settled, the text is normative. Where it is not, the text says so and
links to [Open questions](open-questions.md).

## Start here

- [Overview](overview.md): goals, the founding principle, and what it implies
  for every feature.
- [A tour of Eitri](tour.md): one complete program, walked through line by line.
- [Eitri and Lua](lua.md): how to achieve everything a Lua script does, and
  the few gaps that remain.
- [Roadmap](roadmap.md): the order in which the design is implemented, in
  five stages.
- [Example: an editor configuration](examples/editor-config/): a 635-line,
  nine-module config in the style of a Neovim setup. It is also the reference
  performance workload.

## Language reference

| Chapter | Covers |
|---|---|
| [Lexical structure](language/lexical-structure.md) | Source text, tokens, literals, comments, newlines |
| [Types](language/types.md) | Built-in types, type syntax, aliases, conversions |
| [Bindings and functions](language/bindings-and-functions.md) | `let`, `var`, functions, lambdas, inference |
| [Structs, classes, and enums](language/structs-classes-enums.md) | User-defined types, methods, `@extern` structs, recursive types |
| [Optionals](language/optionals.md) | `?T`, `?.`, `??`, narrowing, `if let`, `guard` |
| [Errors and faults](language/errors.md) | `E!T`, `try`, `try?`, error mapping, faults, `defer` |
| [Pattern matching](language/pattern-matching.md) | `match`, patterns, exhaustiveness |
| [Collections](language/collections.md) | Arrays, fixed arrays, maps |
| [Expressions and control flow](language/expressions.md) | Operators, precedence, `if`, loops, blocks |
| [Strings and formatting](language/strings.md) | Interpolation, escapes, `describe()` |
| [Modules](language/modules.md) | `import`, `pub`, visibility |
| [Grammar](grammar.ebnf) | The full EBNF grammar, with constraints |

## Design notes

Decisions with their rationale and rejected alternatives.

| Note | Covers |
|---|---|
| [Coroutines and async](design/coroutines.md) | `async`/`await`, `spawn` and tasks, cancellation, fault containment, host scheduling and API |
| [Generics and interfaces](design/generics.md) | Type parameters, constraints, interfaces, `any`, specialization |
| [The dynamic type](design/dynamic.md) | `dyn`, type patterns, dynamic access, JSON, inline caches |
| [Runtime features](design/runtime-features.md) | Weak references, cleanup on collection, channels, compiling code at run time, hot reload |

## Implementation

| Chapter | Covers |
|---|---|
| [First supported slice](implementation/first-slice.md) | The M0 subset: accepted syntax and types, rejections, arithmetic, budgets, traces, diagnostic codes |
| [Compiler](implementation/compiler.md) | The compilation pipeline |
| [Virtual machine](implementation/vm.md) | Register VM, instruction format, GC metadata |
| [Execution design](implementation/execution-design.md) | Proposed source-to-execution path, lexer IDs, typed operations, frames, dispatch, JIT and AOT |
| [Typed bytecode](implementation/bytecode.md) | The AArch64-style instruction families, encodings, calls, branches, and verification |
| [VM research and decision ledger](implementation/vm-research.md) | Primary sources, limitations, competing approaches, and experiments for each major execution decision |
| [Embedding](implementation/embedding.md) | The host-binding model: declaration files, values across the boundary, host objects, budgets |
| [Rust API](implementation/rust-api.md) | The `eitri` crate: registration, typed bindings, call lifetimes, async |
| [C API](implementation/c-api.md) | `eitri.h`, the stable ABI: status codes, accessors, `eitri-bindgen` |
| [Identity and hashing](implementation/identity.md) | Hashing invariants, the first implementation, generated equality, map representations, experiments |
| [Performance goals](implementation/performance.md) | What is measured and how |

## Status

- [Implementation tasks](tasks.md): dependency-ordered milestones, task checkboxes,
  completion checks, and research experiments. Start with the measured scalar slice.
- [Open questions](open-questions.md): unresolved design areas, plus
  inconsistencies found between the overview, the grammar, and the examples.

## Conventions in these documents

- Code blocks tagged `eit` are Eitri source.
- **Must**, **must not**, and **may** are normative. Everything else is
  explanation.
- "Fault" always means an unrecoverable runtime failure (see
  [Errors and faults](language/errors.md#faults)). "Error" always means a
  recoverable value of type `E!T`.
- A ⚠️ **Open** callout marks behavior that is not yet decided.
