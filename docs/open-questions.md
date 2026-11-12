# Open questions

This page tracks what is not decided yet. When a question is resolved, move the
answer into the relevant reference chapter and delete it here.

## Lua parity

Every outcome a Lua script can achieve now has an Eitri route; see
[Eitri and Lua](lua.md). What remains is convenience and the standard
library.

### Convenience, not parity

These would make Lua code easier to port. None is needed to reach an outcome,
since each has a route today.

| Feature | Route today |
|---|---|
| Tuples or multiple returns | Return a struct and destructure it |
| Varargs | An array parameter |
| Operator overloading | Named methods |
| Mixed integer and float arithmetic | Explicit conversion |
| Circular imports | A third module, or passing functions in |
| Runtime-wide globals | Module state |
| Generators | Closures returning `?T` |

### Standard library needed for parity

Length, power, floor division and modulo, number parsing, byte access, string
functions and patterns, `utf8`, `math`, `table`, `io`, `os`, and GC control.

## Not yet in the grammar

- Explicit type arguments in expressions (`f<T>(x)`). Deferred: inference
  from arguments and expected types covers it, and adding it would bring back
  the ambiguity of `<` in expressions.
- Attributes with arguments (`@name(...)`). Reserved; the first planned use
  is `@extern(packed)`.
- Packed layouts and explicit offsets for `@extern` types, and storage widths
  other than `i32` for enum fields.

## Open design

- **Hot reload.** The module-level rules are sketched, but each optimization
  needs a replacement or invalidation rule first. See
  [Hot reload: open interactions](design/runtime-features.md#open-interactions).

## Build phase

These need a working compiler and VM, and measurements. Each is marked ⚠️ or
"experimental" in its chapter.

**First experiments**

- The GC frame model: stack maps and split unions, measured for frame size and
  compile time ([VM](implementation/vm.md#garbage-collection))
- The workloads and stress workloads, against Lua 5.4
  ([Performance](implementation/performance.md#workloads))

**Decisions**

- The opcode set and instruction encoding: a first-subset proposal is in
  [Typed bytecode](implementation/bytecode.md), with alternatives and
  promotion experiments in the [research ledger](implementation/vm-research.md)
- The collector beyond the first non-moving mark-sweep ([VM](implementation/vm.md#garbage-collection))
- The Lua equivalent of the reference config, written with the benchmark
  harness

**Experiments, adopted only if measurements justify them**

- A moving collector with header hash states, and eager interning of short
  strings ([Identity and hashing](implementation/identity.md#experiments))
- Specialized map representations ([Identity and hashing](implementation/identity.md#map-representations))
- Shape sharing between generic instantiations ([Generics](design/generics.md#implementation-specialization))
- Closure implementation: heap cells or open upvalues ([VM](implementation/vm.md#closures))

## Resolved

| Question | Decision |
|---|---|
| T013 numeric conversions and shift counts | Float-to-integer truncates, then range-checks; NaN/infinities fault. Integer-to-float and float narrowing use nearest/ties-even without faults, including signed infinity on narrowing overflow. Decimal literals use nearest/ties-even but reject overflow; underflow is allowed. `truncating:` is integer-only. Shift counts may be any integer type. See [Conversions](language/types.md#explicit-numeric-conversions) and [Shifts](language/expressions.md#shifts) |
| Numeric types | Exactly `i8`, `i64`, `u8`, `u64`, `f32`, `f64` (plus `bool`), every width in the name and none target-dependent. Literals default to `i64` and `f64`; no `int`/`float` aliases. See [Types](language/types.md#the-scalar-set) |
| `guard let name = text.trim()` in the original example | `trim()` returns `string`; the example validates with `guard name != ""`. APIs return `E!T` only for operations that can fail and `?T` only when "no value" is normal. See [Errors](language/errors.md#choosing-et-t-or-a-plain-value) |
| Map iteration | `for key, value in m` kept; one name gives keys; arrays get `for i, x in a`; `_` allowed. Maps keep insertion order. Assigning existing entries during iteration is allowed; inserting, removing, or resizing faults. See [Collections](language/collections.md#iteration) |
| `pub let` / `pub var` | Exported bindings must be `let`. Module `var`s stay private; importers change state through exported class instances or functions. One module instance per runtime, initialized once in dependency order; import cycles are compile errors. See [Modules](language/modules.md#exported-bindings-are-let) |
| `mutating` placement | Allowed in `struct` bodies, including `@extern` structs, which are C-layout values. See [Structs](language/structs-classes-enums.md#mutating-methods) |
| `try` at top level | Needs an `Error` mapping; there is no implicit conversion. See [Errors](language/errors.md#top-level-code) |
| `self` vs. bare field names | Locals and parameters shadow fields; fields shadow module bindings; `self` is assignable in `mutating` methods. See [Structs](language/structs-classes-enums.md#names-inside-methods) |
| Struct literals for classes | Same syntax, one allocation each; `..base` copies shallowly into a new object. See [Classes](language/structs-classes-enums.md#classes) |
| Module bindings read before initialization | Compile-time check within the module; referencing a function value counts as calling it. See [Modules](language/modules.md#initialization-order) |
| Type-level functions | `static fn`, called as `Vec2.new()`, resolved at compile time. See [Static functions](language/structs-classes-enums.md#static-functions) |
| Collection semantics | `[]T` and `map[K]V` are references, like classes; `[N]T` is a value. `let` forbids rebinding only; copies are explicit with `copy()`. Core built-in methods are defined. See [Collections](language/collections.md#sharing-collections-are-references) |
| Recursive value types | A type can't contain itself inline; classes, collections, tasks, functions, and `@indirect` enum cases end the cycle. Structs have no `@indirect`. See [Recursive types](language/structs-classes-enums.md#recursive-types) |
| Attributes | `@name` before a declaration, for representation and host binding: `@extern`, `@indirect`. A fixed set; arguments reserved. See [Attributes](language/lexical-structure.md#attributes) |
| Closure captures | Closures capture variables, as Lua upvalues do. `let`s and never-reassigned `var`s are copied; reassigned `var`s become shared cells. Fresh loop bindings per iteration; module bindings aren't captured. See [Captures](language/bindings-and-functions.md#captures) |
| Equality and hashing | Values compare by content, references by identity; `equals()` for content comparison of references. Any type with `==` can be a map key; `NaN` keys fault. Hashing invariants and the first implementation are in [Equality](language/expressions.md#equality-and-ordering) and [Identity and hashing](implementation/identity.md) |
| Field visibility | Members are private to their module unless `pub`. Importers can write a literal only when every field without a default is `pub`, so private fields make static constructors enforceable. See [Visibility](language/structs-classes-enums.md#visibility) |
| Strings | Always valid UTF-8; `bytes` for binary. New `char` type with `'a'` literals. `len()` in bytes, slicing by byte offset, no `s[i]`. Byte-wise ordering, `+` fused into one allocation, `join` for loops. See [Strings](language/strings.md) |
| Lua parity gaps | Weak references and weak-key maps; cleanup on collection through `runtime.on_collect`; `chan T` for task-to-task wakeups; `compiler.load` checked against the expected type. Hot reload is a goal with open interactions (see above). See [Runtime features](design/runtime-features.md) |
| Observing faults in script | `await t` returns `Stopped!T`, with `.Cancelled` and `.Faulted(fault)`; `t.result()` without suspending; `spawn` accepts any call. The task is the containment boundary. See [Faults in tasks](design/coroutines.md#faults-in-tasks) |
| Generics and interfaces | Type parameters with constraints, checked once at the declaration and specialized per type. Explicit conformance; default methods; `any I` for dynamic dispatch. Built-in constraints `Eq`, `Ordered`, `Numeric`, `Formattable`. See [Generics and interfaces](design/generics.md) |
| Coroutines | `async`/`await` on stackful coroutines, `spawn` and `task T`, cancellation at `await` points, the host as scheduler. Generators deferred; switching costs measured in [Performance goals](implementation/performance.md#coroutine-costs). See [Coroutines and async](design/coroutines.md) |
| Dynamic values | `dyn`: implicit in, out through type patterns and typed `if let`. Lua-style access with `null` for missing; no arithmetic; inline caches keyed on type ID. See [The dynamic type](design/dynamic.md) |
| Recovery | `f() else e => fallback` continues with a value; it may diverge, and works in any context. See [Recovery](language/errors.md#recovery-else-without-try) |
| Compile-time analyses | Return inference and the initialization check share one module call graph; SCCs plus bitsets over late bindings only. Reachability is quadratic in the worst case, which a stress workload measures. See [Compiler: module analyses](implementation/compiler.md#module-analyses) |
| Specialization limits | No polymorphic recursion; a host-configurable instantiation cap; large aggregates compare and hash with loops. See [Generics](design/generics.md#implementation-specialization) |
| Ready queue and fault cleanup | One FIFO ready queue per runtime, drained within the host call's budget, leftovers reported and drained by `runtime.drain()`. Cleanup has its own budget and memory reserve; secondary faults are attached to the report. See [The ready queue](design/coroutines.md#the-ready-queue) and [Cleanup after a fault](language/errors.md#cleanup-after-a-fault) |
| Host bindings | Host modules are Eitri declaration files with bodiless `fn`s, bound by name and compiled once at runtime creation. Host calls are indexed. Values cross as native scalars, borrowed views, mirror types, or handles; `retain`/`release` for longer lifetimes. Host classes have properties (host getters and setters) or `@extern` memory fields; stale handles fault. Host functions may return a fault. See [Embedding](implementation/embedding.md) |
| `@extern` types | `@extern struct` is a C-layout value (arrays pass to the host with no copy); `@extern class` is a generation-checked view of host memory. Storage types `i16`, `i32`, `u16`, `u32`, `isize`, `usize`, `ptr`, `cstr` exist only as fields. Scripts never dereference pointers; owned resources are host objects. Layouts are checked at registration. See [Extern structs](language/structs-classes-enums.md#extern-structs) |
| Rust and C APIs | Both sit on the core; the C API is the stable ABI. Rust: typed bindings, borrowed values tied to the call lifetime, `Retained<T>` with queued release, panics become faults. C: status codes, typed accessors, `eitri-bindgen` mirror headers, Eitri type strings for lookup. Runtimes are `Send + !Sync`. See [Rust API](implementation/rust-api.md) and [C API](implementation/c-api.md) |
| Borrowing and reentrancy | Host access to script-visible memory never spans a call back into the runtime. Rust enforces it with the borrow checker (views borrow the call context; reentry needs it mutably); C invalidates pointers on reentry, checked by an epoch in debug builds. See [Embedding](implementation/embedding.md#borrowing-and-reentrancy) |
| Threading | The Rust `Runtime` is thread-bound (neither `Send` nor `Sync`) in the first version, so registered items need no `Send` bounds. One runtime per thread for parallelism. See [Rust API](implementation/rust-api.md#threading) |
| Runtime templates | Host modules registered once into a template; runtimes created from it share the checked descriptors. Used by hosts with many runtimes and by the benchmark's fresh-module case. See [Embedding](implementation/embedding.md#runtimes) |
| Read-only fields | `let` fields can't be assigned after construction; in host classes they are read-only properties. See [Read-only fields](language/structs-classes-enums.md#read-only-fields) |
| Benchmark workload | The reference workload is an editor config the size of a respectable Neovim config (9 modules, 635 lines). Targets: a few µs per small module, under 100 µs for the whole config (proposed), under 1 µs per statusline render (proposed). Nine feature workloads and four stress workloads support it. See [Performance goals](implementation/performance.md) |
| Nested optionals | `?<?T>` is `?T`: optionals collapse through aliases, generics, `pop`, `get`, and `try?`. `m.has(k)`, `len()`, or `E!?T` tell "missing" from "present but `null`". `m.get(k)` returns `?V`. See [Optionals collapse](language/types.md#optionals-collapse) |
| Shadowing | Variables can be redeclared and shadow earlier bindings; predeclared type names can't be used as names. See [Shadowing](language/bindings-and-functions.md#shadowing) |
| Integer literals as floats | Allowed when exactly representable; literals only. See [Types](language/types.md#integer-literals) |
| Labeled argument order | Any order after positional arguments; evaluated as written. See [Calling](language/bindings-and-functions.md#calling) |
| Byte buffers | `u8` exists; `[]u8` is the mutable buffer; `bytes(buf)` and `b.to_buffer()` convert. See [Strings](language/strings.md#bytes) |
| Extensions | `extend T: I { … }` only in the module of `T` or of `I`; no stored fields. See [Extensions](language/structs-classes-enums.md#extensions) |
| Static constants, enum methods | `static let`; enums take methods, static functions, and static constants after their cases. See [Static constants](language/structs-classes-enums.md#static-constants) and [Enum methods](language/structs-classes-enums.md#enum-methods) |
| Integer division and shifts | `/` truncates, `%` follows the dividend; division by zero, `MIN / -1`, and out-of-range shift amounts fault. Float division follows IEEE. See [Division and remainder](language/expressions.md#division-and-remainder) |
| Faults | Adds integer division by zero, `MIN / -1`, bad shift amounts, and running out of memory. See [Faults](language/errors.md#faults) |
| Order of `defer` | Last in, first out. See [`defer`](language/errors.md#defer) |
| Payload patterns | `.Case` ignores any payload; positional before labeled. Range patterns `a..=b`. See [Pattern matching](language/pattern-matching.md#payload-patterns) |
| Narrowing | `!= null`, `== null` with diverging branch, `guard`, `&&`, `\|\|`, `while`. See [Narrowing](language/optionals.md#narrowing) |
| Formatting | Nested strings quoted; shortest round-trip floats; arrays and maps formattable. See [Exact output](language/strings.md#exact-output) |
| Ranges and labeled loops | `Range<T>` values with `contains`, `len`, and `slice(r)`; `outer: for` with `break outer`. See [Ranges](language/expressions.md#ranges) |
| Import position | Imports come first. See [Imports](language/modules.md#imports) |
| Diagnostics | Several errors per run, recovered at statement boundaries; `file:line:col` text plus structured data for hosts. See [Diagnostics](implementation/compiler.md#diagnostics) |
| Optional indexing, multi-line and raw strings | `a?[i]`; `"""…"""` with indentation stripping; `r"…"`. See [Optionals](language/optionals.md) and [Lexical structure](language/lexical-structure.md#multi-line-strings) |

## Inconsistencies

None currently open.
