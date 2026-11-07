# Rust API

**Status:** proposed. Names and signatures may change until the first
release; the semantics follow [Embedding](embedding.md).

The `eitri` crate is the idiomatic way to embed Eitri in a Rust program. It
sits directly on the compiler and VM core, not on the [C API](c-api.md), so a
Rust host pays nothing for the C layer.

## Overview

```rust
use eitri::{Runtime, Config, Budget, Module, Call, Str, RunError};

let mut rt = Runtime::new(Config {
    memory_limit: 64 << 20,
    budget: Budget::steps(1_000_000),
    ..Config::default()
})?;

rt.register(
    Module::new("app", include_str!("app.eit"))
        .function("log", |_cx: &mut Call, message: Str| {
            println!("{}", message.as_str());
        }),
)?;

match rt.run("init.eit", &std::fs::read_to_string("init.eit")?) {
    Ok(()) => {}
    Err(RunError::Compile(diagnostics)) => eprintln!("{diagnostics}"),
    Err(RunError::Error(error)) => eprintln!("script returned an error: {error}"),
    Err(RunError::Fault(fault)) => eprintln!("{}", fault.trace()),
}
```

## Threading

`Runtime` is **thread-bound** in the first version: it is neither `Send` nor
`Sync`, so it stays on the thread that created it. Registered closures,
host state, host objects, loaders, and allocators therefore need no `Send`
bounds, and can hold thread-bound resources such as `Rc` or a UI handle.

Separate runtimes are independent: create one per thread to run scripts in
parallel. A `Send` runtime, which would require `Send` bounds on everything
registered with it, may come later.

## Creating a runtime

```rust
pub struct Config {
    pub memory_limit: usize,                 // bytes; 0 means no limit
    pub allocator: Option<Box<dyn Allocator>>,
    pub budget: Budget,                      // per host-initiated call
    pub cleanup_budget: Budget,              // for defer blocks after a fault
    pub cleanup_reserve: usize,              // bytes reserved for cleanup
    pub instantiation_limit: u32,            // generic instantiations per compile
    pub std_modules: StdModules,             // which eitri: modules are enabled
    pub loader: Option<Box<dyn Loader>>,     // supplies Eitri source modules
}

pub trait Loader {
    /// Returns the source of the Eitri module at `path`, or None if it
    /// doesn't exist. Host modules are resolved before the loader is asked.
    fn load(&mut self, path: &str) -> Option<std::borrow::Cow<'_, str>>;
}

pub trait Allocator {
    fn alloc(&mut self, size: usize, align: usize) -> *mut u8;   // null on failure
    fn free(&mut self, ptr: *mut u8, size: usize, align: usize);
}
```

`StdModules` enables `eitri:compile`, `eitri:json`, and `eitri:runtime`
individually. All are off by default.

## Templates

```rust
let template = RuntimeTemplate::new(config)
    .register(editor_module())?
    .register(lsp_module())?;

let rt = template.instantiate();      // shares the checked module descriptors
```

A template holds checked module descriptors and their bindings. Each
`instantiate` creates a fresh runtime that shares them, so declaration files
are checked once, not per runtime. Bindings registered on a template must be
`Clone`; they are cloned into each runtime.

## Registering host modules

A module is a [declaration file](embedding.md#declaration-files) plus a
binding for each bodiless declaration:

```rust
rt.register(
    Module::new("editor", include_str!("host/editor.eit"))
        .function("notify", notify)
        .function("read_file", read_file)
        .function("map", map)
        .async_function("sleep", sleep)
        .class::<Options>("Options")
            .property("number", |o| o.number, |o, v| o.number = v)
            .property("tab_stop", |o| o.tab_stop, set_tab_stop)
            .getter("version", |o| o.version.clone())
            .done()
        .value("options", options_handle),
)?;
```

`register` checks every binding against its declaration: names, parameter
types, return types, and properties. It returns `RegisterError` with
diagnostics if anything is missing, extra, or mismatched. Registration happens
once per runtime; scripts that import the module never re-parse it.

### Host functions

A host function is any Rust function or closure whose first parameter is
`&mut Call` and whose other parameters and return type implement the binding
traits below. Parameters arrive in declaration order, with defaults already
filled in by the script compiler.

```rust
fn notify(cx: &mut Call, message: Str, level: Level) {
    cx.host::<Ui>().notify(message.as_str(), level);
}

fn read_file(_cx: &mut Call, path: Str) -> Result<String, IoError> {
    std::fs::read_to_string(path.as_str()).map_err(|e| IoError::from_io(path.as_str(), e))
}
```

`cx.host::<T>()` returns the host state registered with
`Runtime::set_host(value)`, so bindings don't need global variables.

### Return types

| Rust return type | Eitri meaning |
|---|---|
| `R` | The declared type `R` |
| `Result<T, E>` where `E` mirrors an Eitri error type | `E!T` |
| `Result<R, Fault>` | `R`, or a fault at the call site |

A host function faults by returning `Err(Fault::new("message"))`. The calling
coroutine faults as if it had called `panic`, its `defer` blocks run, and the
trace names the host function. **A Rust panic inside a host function is
caught** and turned into a fault in the same way; it never unwinds through VM
frames.

## Values

| Eitri type | Parameter type | Return type |
|---|---|---|
| `i8`, `i64`, `u8`, `u64`, `f32`, `f64`, `bool`, `char` | The same Rust type | The same |
| `string` | `Str<'call>` | `String`, `&'static str`, or an `Interned` string |
| `bytes` | `Bytes<'call>` | `Vec<u8>` |
| `?T` | `Option<T>` | `Option<T>` |
| `E!T` | `Result<T, E>` | `Result<T, E>` |
| `struct`, `enum` | A type with `#[derive(eitri::Struct)]` or `#[derive(eitri::Enum)]` | The same |
| `@extern struct` | `T`, a `#[repr(C)]` type with `#[derive(eitri::Extern)]` | `T` |
| `[]T` of an `@extern struct` | `ExternSlice<'call, T>`; `cx.slice(&s)` gives a `&[T]` with no copy | — |
| `@extern class` | `HostObject<T>`; `cx.get(&o)` and `cx.get_mut(&o)` give `&T` and `&mut T` | `HostObject<T>` |
| Host `class` | `HostObject<T>`, accessed through the call context as above | `HostObject<T>` |
| Script class, `[]T`, `map[K]V`, `dyn`, `chan T`, `task T` | `Handle<'call, T>`, with typed views such as `ArrayRef<'call, T>` and `MapRef<'call, K, V>` | `Handle` or a new value built through the runtime |
| `fn(A…) -> R` | `Callback<'call, (A…,), R>` | — |
| `async fn(A…) -> R` | `AsyncCallback<'call, (A…,), R>` | — |

Mirror types are checked against the declaration at registration: field
names, field types, and enum cases in order.

`#[derive(eitri::Extern)]` on a `#[repr(C)]` type also records its size,
alignment, and field offsets, and registration checks them against the
layout Eitri computes for the `@extern` declaration. Storage-type fields map
to the matching Rust types (`i32`, `u16`, …), `ptr` to a raw pointer, and
`cstr` to `*const c_char`.

```rust
#[derive(eitri::Struct)]
struct Position { line: i64, column: i64 }

#[derive(eitri::Enum)]
enum Level { Debug, Info, Warn, Error }

#[derive(eitri::Enum)]
enum IoError {
    NotFound { path: String },
    Denied { path: String },
    Other { message: String },
}
```

### Borrowed values and the call lifetime

`Str<'call>`, `Bytes<'call>`, `Handle<'call, T>`, and `Callback<'call, …>`
borrow from the call that received them. The lifetime ties them to that call,
so **keeping one past the call doesn't compile**. This is the
[borrowed-for-the-call rule](embedding.md#retaining-references), enforced by
the borrow checker.

### Borrowing and reentrancy

A host function may call back into the runtime, and the script code that
runs could resize an array or change an object the host function is looking
at. [Embedding](embedding.md#borrowing-and-reentrancy) states the rule: host
access to script-visible memory never spans a call back into the runtime.

In Rust this is checked by the compiler:

- Views into script-visible memory are **borrowed from the call context**:
  `cx.slice(&s)`, `cx.get(&o)`, `cx.get_mut(&o)`, `cx.host::<T>()`, and
  `s.as_str()` on a `Str` all hold a borrow of `cx`.
- Calling back into the runtime (`cx.call`, `cx.start`, and the like) takes
  `&mut cx`.
- So holding a view across a reentrant call is a borrow error:

```rust
fn sum_then_notify(cx: &mut Call, verts: ExternSlice<Vertex>, cb: Callback<(), ()>) {
    let total: f32 = cx.slice(&verts).iter().map(|v| v.x).sum();   // borrow ends here
    cx.call(&cb, ())?;                       // reentry: the script may resize the array
    let len = cx.slice(&verts).len();        // fetch again: sees the current state
}
```

Property getters and setters receive `&T` or `&mut T` but no call context, so
they can't call back into the runtime at all.

### Retaining

To keep a value after the call returns, retain it:

```rust
fn map(cx: &mut Call, mode: Mode, keys: Str, action: Callback<(), ()>,
       desc: Str, buffer: Option<i64>) {
    let action = action.retain();          // Retained<Callback<(), ()>>, no lifetime
    cx.host::<Ui>().keymaps.insert((mode, keys.to_string(), buffer), action);
}
```

- `Retained<T>` has no lifetime and keeps the value alive for the collector.
- **Dropping it queues the release**; the runtime processes queued releases at
  its next safe point. `Drop` therefore needs no access to the runtime.
- Calling a retained callback needs the runtime:
  `rt.call(&action, ())` returns `Result<R, Fault>`.

## Host objects

A host class's objects are stored in the runtime's host-object table and
reached through `HostObject<T>` handles:

```rust
let options = rt.host_object(Options::default());   // HostObject<Options>
rt.get_mut(&options).number = true;                 // host-side access
let removed: Options = rt.destroy(options);         // handles become stale
```

- Property getters and setters receive `&T` or `&mut T` for the object, and
  no call context. Host methods receive the call context and reach the object
  through it, under the [borrowing rule](#borrowing-and-reentrancy).
- After `destroy`, any script access through a handle to that object
  **faults** with "host object no longer valid".
- `.class::<T>(name).on_unreferenced(|rt, obj| …)` is called when the
  script's last reference to an object is collected. It runs host code, never
  script code.

## Running scripts

```rust
pub enum RunError {
    Compile(Diagnostics),
    Error(ScriptError),        // the top level returned an Error value
    Fault(Fault),
}

rt.run(name, source) -> Result<(), RunError>        // compile, check, and run
rt.compile(name, source) -> Result<Chunk, Diagnostics>
rt.run_chunk(&chunk) -> Result<(), RunError>
```

`Diagnostics` holds structured diagnostics (severity, code, message, source
ranges) and formats as `file:line:col` text. `Fault` holds the message, the
source trace, and any secondary faults from cleanup.

## Calling script functions

```rust
let on_save = rt.function::<fn(i64) -> bool>("init", "on_save")?;   // checked once
let keep: bool = on_save.call(&mut rt, buffer_id)?;                 // Result<bool, Fault>
```

The type parameter is checked against the function's Eitri type at lookup,
and the returned `ScriptFn` calls it by index afterwards. Only `pub`
functions can be looked up. Each call is a host-initiated call with its own
budget.

## Async host functions and tasks

```rust
fn sleep(cx: &mut Call, ms: i64) -> Async<()> {
    if ms <= 0 {
        return Async::Ready(());               // fast path: no suspension
    }
    let token = cx.suspend();                  // Suspension
    cx.host::<Ui>().timers.push(ms, token);
    Async::Pending
}

// Later, from the host's event loop:
let report = rt.resume(token, ())?;            // Report
```

| Method | Does |
|---|---|
| `cx.suspend()` | Suspends the calling coroutine and returns a one-shot `Suspension` |
| `cx.on_cancel(token, f)` | Registers a host hook that runs if script code cancels the task |
| `rt.resume(token, value)` | Resumes with a value of the declared return type |
| `rt.cancel(token)`, `rt.cancel_task(task)` | Cancels a task |
| `rt.start(&async_callback, args)` | Starts a task for a script `async` callback |
| `rt.drain()` | Runs the ready queue left non-empty by an exhausted budget |

Each of these returns a `Report`: the primary task's ID and outcome, every
other task that finished, secondary faults, and the ready-queue length.
Using a spent or stale `Suspension` returns `Err(ApiError::StaleToken)`.

## Worked example: binding the editor

The editor in [examples/editor-config](../examples/editor-config/) registers
[`host/editor.eit`](../examples/editor-config/host/editor.eit). An abridged
binding:

```rust
struct Editor {
    keymaps: HashMap<(Mode, String, Option<i64>), Retained<Callback<(), ()>>>,
    autocmds: Vec<(Event, String, Retained<Callback<(AutocmdEvent,), ()>>)>,
    // ...
}

fn register_editor(rt: &mut Runtime) -> Result<(), RegisterError> {
    let options = rt.host_object(Options::from_editor_defaults());

    rt.register(
        Module::new("editor", include_str!("host/editor.eit"))
            .function("map", |cx: &mut Call, mode: Mode, keys: Str,
                              action: Callback<(), ()>, desc: Str, buffer: Option<i64>| {
                let ed = cx.host::<Editor>();
                ed.keymaps.insert((mode, keys.to_string(), buffer), action.retain());
                ed.describe_key(keys.as_str(), desc.as_str());
            })
            .function("on", |cx: &mut Call, event: Event,
                             callback: Callback<(AutocmdEvent,), ()>, pattern: Str, group: Str| {
                cx.host::<Editor>().autocmds.push((event, pattern.to_string(), callback.retain()));
            })
            .function("read_file", |_cx: &mut Call, path: Str| -> Result<String, IoError> {
                std::fs::read_to_string(path.as_str())
                    .map_err(|e| IoError::from_io(path.as_str(), e))
            })
            .class::<Options>("Options")
                .property("number", |o| o.number, |o, v| { o.number = v; o.redraw = true; })
                .property("tab_stop", |o| o.tab_stop, |o, v: i64| o.tab_stop = v.clamp(1, 32))
                .property("status_line", |o| o.status_line.clone(),
                          |o, v: Option<Callback<(), String>>| o.status_line = v.map(|c| c.retain()))
                // ... one property per field
                .getter("version", |o| o.version.clone())
                .done()
            .value("options", options)
            // ... the remaining functions
    )
}

// When the user presses a mapped key:
fn on_key(rt: &mut Runtime, key: &(Mode, String, Option<i64>)) {
    let action = rt.host::<Editor>().keymaps.get(key).cloned();
    if let Some(action) = action {
        if let Err(fault) = rt.call(&action, ()) {
            show_error(fault.trace());
        }
    }
}
```
