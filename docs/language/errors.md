# Errors and faults

The current implementation supports named `E!T` results in script functions
and locals, implicit and explicit success, explicit errors, matching, equality,
and `void` success, including fields of structs, classes, and enums. Compound
result type syntax, nested result
patterns, `try?`, error mapping, and host result signatures remain future work.
Plain `try` propagates matching errors from script functions.

Eitri separates two kinds of failure:

| | Error | Fault |
|---|---|---|
| Kind | A value of type `E!T` | An abort |
| Recoverable in script | Yes | No |
| Examples | File not found, parse failure, missing map key via `get` | Out-of-bounds index, overflow, `panic` |
| Reaches the host as | A returned `.Err` value | A fault with a source trace |

There is no throwing and no catching.

## Results: `E!T`

`E!T` holds either an error `E` (case `.Err`) or a success `T` (case `.Ok`).
`E` must be a named type.

```eit
enum LoadError {
    NotFound(path: string)
    Invalid(message: string)
}

fn parse_config(text: string) -> LoadError!Config { ... }
```

A `T` converts implicitly to `E!T`, so returning a success needs no wrapper.
Returning an error does:

```eit
fn load(path: string) -> LoadError!Config {
    if path == "" {
        return .Err(.Invalid(message: "empty path"))
    }
    return default_config          // same as .Ok(default_config)
}
```

### `void` successes

In an `E!void` function, a bare `return` or falling off the end means success.
A non-`void` success type requires a value on every path that completes.

## `try`

`try expr` unwraps a success, or returns the error early:

```eit
fn load(path: string) -> LoadError!Config {
    let text = try read_text(path)       // read_text: LoadError!string
    return parse_config(text)
}
```

- `try` returns from the **innermost function or lambda**.
- The operand's error type must **exactly match** the enclosing function's
  error type. There is no implicit error conversion.
- `try` binds to a single postfix expression. To use the unwrapped value in a
  larger chain, parenthesize:

  ```eit
  let title = (try load(path)).title
  ```

- With an `async` call, `try` comes before `await`:

  ```eit
  let text = try await app.fetch(url)
  ```

### Error mapping: `try ... else`

When the error types differ, map the error:

```eit
let text = try app.read_file(path) else e => .NotFound(path: path)
let text = try app.read_file(path) else _ => .NotFound(path: path)
```

- The mapping runs **only on failure**.
- It binds the original error to a name, or discards it with `_`.
- Its body may be an expression or a block.
- It must produce the enclosing function's error type, **or diverge**.

```eit
let text = try app.read_file(path) else e => {
    app.log("read failed: ${e}")
    .NotFound(path: path)
}
```

The syntax differs from `guard`'s `else {` because `else` is followed by a name
and `=>`, never directly by `{`.

## `try?`

`try? expr` discards the error and yields `?T`:

```eit
let config = try? load("init.eit") ?? default_config
```

Because `try?` binds to one postfix expression, the line above means
`(try? load("init.eit")) ?? default_config`.

When `T` is already optional, the optionals collapse: `try?` on `E!?T` gives
`?T`, and an error and a successful `null` both become `null`. Use `match`
to tell the three states apart.

`try?` does not return early, so it can be used in any function.

## Recovery: `else` without `try`

Writing the error mapping **without** `try` recovers with a fallback value
instead of propagating:

```eit
let text = app.read_file(path) else _ => ""

let config = load("init.eit") else e => {
    app.log("using defaults: ${e}")
    default_config
}
```

| Form | On error |
|---|---|
| `try f()` | Returns the error unchanged |
| `try f() else e => mapped` | Returns a mapped error |
| `f() else e => fallback` | Continues with `fallback`, a value of the success type |
| `try? f()` | Continues with `null` |

- Unlike `try? f() ?? fallback`, recovery keeps the error available, and it
  keeps an error apart from a successful `null`.
- The fallback may diverge instead (`return`, `break`, `continue`,
  `panic`), which makes it a one-line guard on errors:

  ```eit
  let n = i64.parse(input) else _ => return
  ```

- Recovery never propagates, so it is allowed in any function, in `defer`
  blocks, and in top-level code.
- Like `try`, the operand is a single postfix expression, optionally with
  `await`: `await fetch(url) else _ => cached`.

## Top-level code

A module's top-level code behaves as the body of an implicit `Error!void`
function, and the host receives that result. A top-level `try` therefore
requires its operand's error type to be `Error`, or a mapping to `Error`.
There is no implicit conversion:

```eit
let theme = try load_theme("theme.txt")      // error: LoadError is not Error

let theme = try load_theme("theme.txt") else e => Error {
    message: "theme: ${e}",
}
```

`Error`'s `location` default runs `caller_location()` at this construction
site, so the error the host receives points at this line.

## The standard `Error` type

```eit
struct Error {
    message: string
    location: ?SourceLocation = caller_location()
}
```

`caller_location()` records the location where the `Error` is constructed. It
is **not** a stack trace. Because defaults are evaluated at each call site, a
helper can forward its own caller's location through a defaulted parameter:

```eit
fn config_error(
    message: string,
    location: ?SourceLocation = caller_location(),
) -> Error {
    return Error { message: "config: ${message}", location: location }
}
```

## Choosing `E!T`, `?T`, or a plain value

Standard library and host APIs follow one rule:

| Return | When |
|---|---|
| `E!T` | The **operation itself can fail**: a file is missing, text doesn't parse |
| `?T` | "No value" is a **normal outcome** of the question asked, as with `find`, `pop`, and `m.get` |
| `T` | Neither. The operation always produces a valid result |

When the value type is itself optional, such as `pop()` on a `[]?i64`, the
optionals [collapse](types.md#optionals-collapse): the result is `?i64`.

An API never returns `E!T` or `?T` just to report an input the caller might
not like. That check belongs to the caller who cares, so its cost is paid only
there.

For example, `trim()` returns `string`. An empty result is a valid string, and
most callers don't care whether it is empty. A caller that does validates
explicitly:

```eit
let name = text.trim()
guard name != "" else {
    return .Err(.Invalid(message: "empty theme file"))
}
```

If `trim()` returned an error instead, every call would need `try` or `try?`,
and every call would pay for a result value and a branch, to serve the few
callers that want the check.

## Faults

A fault is unrecoverable. The following fault:

- Indexing an array out of bounds.
- Integer overflow in `+`, `-`, `*` (use `&+`, `&-`, `&*` to wrap).
- A failed checked conversion, such as `u8(x)` when `x` is out of range.
- Integer division or remainder by zero, and signed `MIN / -1`.
- A shift amount outside the operand's bit width.
- Running out of memory, as limited by the host.
- Indexing a map with a missing key (`m[k]`). Use `m.get(k)` to get `null`
  instead.
- Exhausting the execution budget set by the host.
- Calling `panic("...")`.

A fault from running out of memory terminates the coroutine that was running
when the allocation failed.

When a fault happens:

1. The coroutine it happens in stops. Code that isn't inside a spawned task
   runs in the coroutine the host call started, so for plain scripts this
   means the current **host-initiated call** stops.
2. Pending `defer` blocks in that coroutine run, under the cleanup rules
   below.
3. The host receives the fault with a source trace.

### Cleanup after a fault

Cleanup is arbitrary code, so it gets its own limits. The guarantee to the
host is that **control always comes back in bounded time**, even when
cleanup misbehaves.

- **Cleanup budget.** `defer` blocks that run because of a fault or a
  cancellation share a separate, host-configured cleanup budget. If it runs
  out, the remaining `defer` blocks of that coroutine are skipped, and the
  report says so.
- **Memory reserve.** The runtime keeps a small reserve of memory that only
  cleanup may use, so a fault caused by running out of memory can still run
  its `defer` blocks. If cleanup exhausts the reserve too, that is a
  secondary fault.
- **Secondary faults.** If a `defer` block faults during cleanup, that block
  stops, and the remaining `defer` blocks still run, each on its own. Every
  secondary fault is attached to the primary fault's report, so none is lost.
- **Nothing else continues.** After cleanup, the coroutine is Faulted. It
  never resumes.

So every host call into Eitri has exactly two outcomes: a value (which may be
an `.Err`) or a fault. A fault in a [spawned task](../design/coroutines.md#faults-in-tasks)
terminates only that task, and is reported to the host separately. Code that
awaits a faulted task receives the fault as a value, so a script can contain a
failing subsystem by running it in a task.

## `defer`

```eit
fn process(path: string) -> LoadError!void {
    let handle = try open(path)
    defer { handle.close() }

    let data = try handle.read()
    ...
}
```

A `defer` block runs on **every** exit from its scope: normal completion,
`return`, `break`, `continue`, an early return from `try`, and faults.

Several `defer` blocks in one scope run in **reverse order of declaration**,
last in, first out, as in Swift, Go, and Zig. Resources are released in the
opposite order to how they were acquired.

`try` and `try ... else` are not allowed inside a `defer` block, since a
deferred block has nowhere to return an error to. `await` is not allowed
either, because cleanup must not suspend.

When a suspended coroutine is cancelled, its pending `defer` blocks run from
the innermost scope outward. A suspended coroutine that is simply abandoned and
garbage collected **never exits, so its `defer` blocks never run**. See
[Coroutines and async](../design/coroutines.md#memory-and-lifetime).
