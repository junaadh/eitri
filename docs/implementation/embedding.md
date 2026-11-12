# Embedding

Eitri is designed to be embedded. The host owns the runtime, decides what
scripts can see, and limits what they can spend.

This chapter defines the host-binding model: how host modules are declared,
how values cross the boundary, and what each side may rely on. The
[Rust API](rust-api.md) and the [C API](c-api.md) are two spellings of the
same operations.

For the implemented M0 surface, see the [security review](security.md):
`Context` currently provides fuel and frame-depth limits, but no memory
quota, custom allocator, cleanup reserve, tasks, or C ABI. The facilities
below describe the broader embedding design.

## Runtimes

- A **runtime** is an isolated instance. Two runtimes share no script state.
- Each runtime can use a **custom allocator** and a **memory limit**.
- Each runtime has an **execution budget**, checked at backward branches and
  calls, and a separate **cleanup budget** and memory reserve for cleanup
  after faults. See [Cleanup after a fault](../language/errors.md#cleanup-after-a-fault).
- The host registers its modules when it creates the runtime (below). Scripts
  run afterwards.
- **Templates.** A host that creates many runtimes, one per plugin or per
  benchmark run, registers its modules once into a **runtime template**.
  Module descriptors are immutable, so creating a runtime from a template
  shares them without re-checking any declaration file.

## Running a script

The host passes a source string or an `init.eit` file. The runtime compiles,
fully checks, and runs it. The script's top level is an implicit `Error!void`
body, so the host gets back one of:

- **success**,
- **an `Error` value** (returned by a top-level `try` or explicitly), or
- **a fault**, with a source trace.

The same two-outcome rule (value or fault) holds for every host call into
Eitri, including callbacks the script registered.

## Host modules

Imports are host-controlled. A script can only reach the outside world
through modules the host registers, plus the standard `eitri:` modules the
host enables.

### Declaration files

The host describes each module in an **Eitri declaration file**. It uses the
normal grammar with one addition: a `fn` without a body is implemented by the
host.

```eit
// editor.eit, registered by the host as module "editor"
pub enum Mode {
    Normal
    Insert
    Visual
    Replace
    Command
    Terminal
}
pub enum Level {
    Debug
    Info
    Warn
    Error
}

pub struct Position {
    pub line: i64
    pub column: i64
}

pub class Options {
    pub number: bool                 // property: host getter and setter
    pub tab_stop: i64
    pub let version: string          // read-only property: getter only
}

pub let options: Options             // value supplied at registration

pub fn notify(message: string, level: Level = .Info) -> void     // host
pub fn read_file(path: string) -> IoError!string                 // host
pub async fn sleep(ms: i64) -> void                              // host, may suspend

pub fn warn(message: string) -> void {                          // written in Eitri
    notify(message, level: .Warn)
}
```

- A bodiless `fn`, a host `class`, and a `pub let` without an initializer are
  allowed **only** in modules the host registers. A script can't declare them.
- A host module can mix host functions with helpers written in Eitri.
- The host binds an implementation to every bodiless declaration **by name**.
  Registration fails if a declaration is left unbound, if an implementation
  has no declaration, or if a signature doesn't match.

### Registration

Registration happens once, when the runtime is created:

1. The declaration file is parsed and checked like any module.
2. Each implementation is matched to its declaration, and each host function
   gets an index in the runtime's host function table.
3. The result is stored as a compiled **module descriptor**.

Scripts that import the module read its descriptor. They never re-parse the
declaration file, so host modules add nothing to per-script compile time.

Bindings can be written by hand, or generated: a Rust macro can produce the
declaration file and the bindings from annotated Rust items. The declaration
file stays the canonical form, because every host language can produce one.

### Import resolution

- An import path names either a registered host module or an Eitri source
  module that the host's loader supplies. The host decides both.
- `eitri:` paths (`eitri:compile`, `eitri:json`, `eitri:runtime`) are
  standard modules, available only if the host enables each one.
- A path that doesn't resolve is a compile error.

## Calls into the host

- **A host call compiles to one instruction**: call host function number N.
  The index is fixed at compile time, so there is no lookup by name at run
  time.
- **The script compiler fills in defaults and reorders labeled arguments.**
  The host always receives every argument, in declaration order.
- **Arguments arrive in the VM frame's slots.** The host reads them, and
  writes its result, through a typed call context. Nothing is copied into a
  separate argument list.
- **The host may call back into the runtime** during a call: calling script
  functions, starting or resuming tasks. Nested work shares the outermost host
  call's budget. Only suspended coroutines can be resumed.

## Values across the boundary

| Eitri type | Host sees | Notes |
|---|---|---|
| `i8`, `i64`, `u8`, `u64`, `f32`, `f64`, `bool`, `char` | The native value | Read straight from the slot |
| `string`, `bytes` | A borrowed view: pointer and length | Valid only during the call. A returned string is copied into the runtime, or is a string the host interned at registration |
| `?T` | An optional of `T` | |
| `E!T` | A result of `T` or `E` | `E` must be declared in Eitri |
| `struct`, `enum` | A mirror type matching the declaration | Copied in and out, field by field |
| `@extern struct` | A C-layout value, copied | The layout is checked at registration |
| `[]T` of an `@extern struct` | A borrowed pointer and length | No copy: the array's elements are already in C layout |
| Host `class` | An **object handle** | See [Host objects](#host-objects) |
| Script class, `[]T`, `map[K]V`, `dyn`, `chan T`, `task T`, `weak T` | A **reference handle** | Read and written through runtime API calls, without copying. Valid during the call unless retained |
| `fn(…) -> R` | A **callback handle** | Called through the runtime API |
| `async fn(…) -> R` | A callback handle | Calling it starts a task and returns that task's report |

### Borrowing and reentrancy

A host function may call back into the runtime, and the script code it runs
could resize an array or change an object the host function is looking at.
The rule that prevents aliasing:

> **Host access to script-visible memory never spans a call back into the
> runtime.** Borrowed views of arrays, host-object memory, strings, and host
> state are valid only until the host function next calls into the runtime,
> or returns, whichever comes first. After a reentrant call, the host
> fetches them again.

- **Rust enforces this at compile time**, at no runtime cost: views are
  borrowed from the call context, and calling into the runtime needs the call
  context mutably, so the borrow checker rejects holding both. See
  [Rust API: borrowing](rust-api.md#borrowing-and-reentrancy).
- **C can't enforce it statically.** Every pointer from an argument accessor
  or `eitri_self` is invalidated by a reentrant call. Debug builds detect use
  after a reentrant call with an epoch counter. See
  [C API](c-api.md#borrowing-and-reentrancy).
- Handles (not views) stay valid across reentry: a handle is an index, so
  fetching through it after the call sees the current state.

### Retaining references

Handles passed to a host function are **borrowed for that call only**.

- To keep one longer, such as a keymap's action, the host calls `retain`,
  which pins the value for the collector, and later `release`.
- Retained handles are GC roots.
- Using a borrowed handle after its call returns is a host API error, caught
  in debug builds.
- Handles still retained when the runtime is destroyed are reported as leaks
  in debug builds.

## Host objects

A host `class` is backed by an object the host owns. Its fields come in two
kinds:

| Kind | Declared as | Access compiles to | Use when |
|---|---|---|---|
| **Property** | `pub number: bool` in a host `class` | One host call: getter or setter number N | Setting a value has side effects or needs checking, such as an option that triggers a redraw |
| **Memory field** | Fields of an `@extern class`, with C layout checked at registration | A generation check, then one load or store at a fixed offset | Plain data the host reads directly |

- `pub let x: T` inside a host class is a **read-only property**: it has a
  getter and no setter.
- Host classes can declare methods without bodies, such as `pub fn close()`,
  implemented by the host like functions, with the object handle as the
  receiver.
- **Identity.** An object handle is a slot index plus a generation number.
  `==` compares handles.
- **The host owns the object.** When the host destroys it, every handle to it
  becomes stale. Accessing a stale handle **faults** with "host object no
  longer valid", because a silent wrong value would be worse.
- **Collection.** When the script's last reference to a host object is
  collected, the host receives a `drop` callback. That runs host code, never
  script code, so it is consistent with the collector never running scripts.
- Host classes can't be constructed with a literal from script code. Scripts
  get them from host functions, host values, or static functions the host
  declares.

### `@extern` structs and classes

`@extern` gives a type C layout. An `@extern struct` is a value with C
layout; an `@extern class` is a view of an object in host memory, reached
through a generation-checked handle. Storage types (`i16`, `i32`, `u16`,
`u32`, `isize`, `usize`, `ptr`, `cstr`) describe C fields exactly, pointers
are never dereferenced by scripts, and owned resources are host objects rather
than fields. The full rules are in
[Extern structs](../language/structs-classes-enums.md#extern-structs).

At registration, the host's real layout (size, alignment, and every field
offset) is checked against the layout Eitri computes from the declaration.

## Errors and faults from host code

- **Expected failures are values.** A host function that can fail declares an
  `E!T` return type and returns `.Err`.
- **Host bugs fault.** A host function may return a fault with a message. It
  behaves like `panic` at the call site: the calling coroutine faults, its
  `defer` blocks run, and the trace includes the host function's name. This is
  Eitri's version of Lua's `lua_error`.

## Calling scripts from the host

The host looks up a script's `pub` function once, giving the type it expects.
The lookup checks that type and returns a typed handle; calls through the
handle use an index, with no lookup by name:

```
handle = lookup(runtime, module: "init", name: "main", type: fn(i64) -> i64)
result = call(runtime, handle, 42)      // a value, an .Err, or a fault
```

Only `pub` functions can be looked up. Each call is a host-initiated call: it
gets the execution budget and returns a value or a fault.

## Coroutines

The host is the scheduler. Eitri has no event loop of its own. The full
semantics are in [Coroutines and async](../design/coroutines.md#host-api);
in short:

- Invoking a script callback of `async` type starts a task.
- A host `async` function receives a **suspension token**. It either returns
  a value immediately (no switch), or keeps the token and suspends, optionally
  registering an `on_cancel` hook.
- `resume(token, value)` resumes with a value of the declared return type.
  Errors are `.Err` values.
- `cancel(token)` and `cancel(task)` cancel tasks.
- Every start, resume, and cancel returns a **report**: the primary task's
  ID and outcome (`Suspended`, `Completed`, `Cancelled`, or `Faulted`), every
  other task that finished during the call, any secondary faults from
  cleanup, and how many coroutines are still in the ready queue.
- `drain` runs the ready queue when a previous call left it non-empty because
  its budget ran out.
- Tokens are one-shot and generation-checked. Misuse is a host API error,
  never a script fault.
- A script callback passed to a host function with a plain `fn` type can't
  suspend. This is checked at compile time.
- The host must complete or cancel outstanding tasks before destroying a
  runtime. The collector frees memory but never runs `defer` blocks.

## Faults and traces

A fault aborts the coroutine it happens in, runs its pending `defer` blocks
under the cleanup rules, and reaches the host with a source trace built from
debug metadata. Host functions on the stack appear in the trace by name.
