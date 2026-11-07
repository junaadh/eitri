# Roadmap

The language design covers full Lua parity. The implementation lands in
stages, so performance claims come from a working compiler and VM as early as
possible. Each stage ends with its features measured against the
[performance workloads](implementation/performance.md).

The [implementation task list](tasks.md) breaks these stages into executable
milestones with dependencies and completion checks. Its first milestone is a
small scalar source-to-result path, before the full Stage 1 runtime.

## Stage 1: core runtime

The smallest runtime that can run the
[reference config](examples/editor-config/) without its dynamic and async
parts.

| Area | Included |
|---|---|
| Front end | Lexer, parser, name resolution, type checking with local inference, the module analyses |
| Types | The seven scalars, `char`, `string`, `bytes`, structs, classes, enums with `@indirect`, `?T`, `E!T`, `[]T`, `[N]T`, `map[K]V`, ranges |
| Language | `let`/`var`/shadowing, functions with defaults and labels, lambdas and closures, `static` members, enum methods, `match` with all patterns except type patterns, `if let`/`guard`, loops with labels, `try`, `try?`, recovery, `defer`, faults, string interpolation |
| Modules | Imports, `pub`, initialization order, Eitri source modules through a loader |
| Embedding | Host declaration files with functions, properties, and values; runtime templates; budgets; the Rust API without async |
| VM | Register bytecode, stack maps, non-moving mark-sweep collector |
| Tools | `eitri` command-line runner, diagnostics |
| Measured | The reference config (without `dyn` and `compiler.load`), the feature workloads, and the stress workloads, against Lua 5.4 |

## Stage 2: types

Generics and interfaces (specialization with its limits), `any`, extensions,
`@extern` types and storage types, and the C API with `eitri-bindgen`.

## Stage 3: concurrency

`async`/`await`, `spawn` and tasks, channels, fault containment, the ready
queue, and async host functions.

## Stage 4: dynamic and runtime features

`dyn` and type patterns, `eitri:json`, `compiler.load`, weak references,
cleanup on collection. Lua parity is complete at the end of this stage.

## Stage 5: tooling and reload

Hot reload (once its [open interactions](design/runtime-features.md#open-interactions)
are settled), and the experiments listed in
[Open questions](open-questions.md#build-phase).
