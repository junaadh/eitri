# Design note: runtime features

**Status:** weak references, cleanup on collection, channels, and compiling
at run time are decided. Hot reload is a goal whose semantics are sketched
but whose interaction with optimizations is still open; see
[Hot reload](#hot-reload). These close the Lua parity gaps: outcomes a Lua
script can achieve that Eitri had no route for. See [Eitri and Lua](../lua.md). The
sixth gap, observing a fault in script, is covered by
[fault containment](coroutines.md#faults-in-tasks) in the coroutines note.

Each feature costs something only in code that uses it.

## Weak references

```eit
let cache: map[string]weak Texture = [:]
cache["logo"] = weak(texture)

if let t = cache["logo"].get() {
    draw(t)
}
```

- **`weak T`** holds a reference to `T` without keeping it alive. `T` must be
  a reference type: a class, `[]T`, `map[K]V`, `task T`, `chan T`, or a
  closure.
- `weak(x)` creates one. `w.get()` returns `?T`: `null` once the target has
  been collected.
- `weak T` supports `==` by identity of the target, so it can be a map key.

### Weak-key maps

```eit
let extra: map[weak Node]Metadata = [:]
extra[node] = Metadata { ... }
```

Writing `weak K` in the key position makes a weak-key map, like Lua's
`__mode = "k"`. It attaches data to objects without keeping them alive.

- An entry disappears once its key is collected.
- The value is kept alive only while its key is. These are **ephemeron**
  semantics: a value that refers back to its own key doesn't keep the entry
  alive.
- Lookups and assignments take a plain `K`; the map stores it weakly.
- Iteration skips entries whose key has been collected. Removals made by the
  collector never trigger the [iteration fault](../language/collections.md#changing-a-collection-while-iterating-it).
- `len()` counts only entries whose key is still alive.

**Cost:** weak references and weak maps add work to the collector. Code that
doesn't use them pays nothing.

## Cleanup on collection

The garbage collector never runs script code. Cleanup on collection is
therefore queued and run later, following JavaScript's
`FinalizationRegistry`:

```eit
import runtime from "eitri:runtime"

let id = gpu.create_texture(data)
let texture = Texture { id: id }
runtime.on_collect(texture, () => gpu.release(id))
```

- When `texture` becomes unreachable, the cleanup function is **queued**,
  not run.
- The queue is drained at a safe point: by default, at the end of each
  outermost host call. The host can drain it at another time instead, such as
  once per frame.
- Each cleanup runs like a host-initiated call: a fault in it is reported to
  the host and doesn't affect other cleanups.
- **The cleanup never receives the object**, only what it captured, so the
  object can't be brought back to life.
- **Caveat:** a cleanup that captures the object itself keeps the object
  reachable, so it is never collected. JavaScript has the same caveat.

`defer` and explicit `close()` methods remain the right tool for cleanup at a
known point. `on_collect` is for resources whose lifetime only the collector
knows.

**Cost:** one registration per object that uses it. The queue is empty
otherwise.

## Channels

`chan T` is an unbounded FIFO queue that tasks use to wake each other. It is
how script code builds signals, work queues, and its own schedulers.

```eit
let jobs: chan Job = chan()

jobs.send(job)                      // never suspends
let next = await jobs.receive()     // ?Job
let maybe = jobs.try_receive()      // ?Job, without suspending
jobs.close()
```

| Operation | Returns | Behavior |
|---|---|---|
| `chan()` | `chan T` | A new, open, empty channel. Needs an expected type |
| `c.send(v)` | `void` | Appends `v`. Never suspends, so plain functions and host callbacks can send. Faults if the channel is closed |
| `await c.receive()` | `?T` | The next value. Suspends while the channel is empty and open. `null` once it is closed and empty |
| `c.try_receive()` | `?T` | The next value, or `null` if none is available right now |
| `c.close()` | `void` | No more values can be sent. Waiting receivers get `null` once the remaining values are taken |
| `c.len()` | `i64` | Number of queued values |

- **Receivers are served in FIFO order**, in the order they started waiting.
- **A woken receiver runs before control returns to the host.** It goes
  through the runtime's [ready queue](coroutines.md#the-ready-queue), the
  same FIFO queue that resumes a finishing task's waiters, with the same
  ordering and budget rules.
- Channels are references: `==` compares identity.

A script-written scheduler is a set of tasks, each awaiting its own channel,
plus code that decides which channel to send to next.

**Cost:** the ready queue is empty unless something was woken. Code that
doesn't use channels never touches it.

## Compiling code at run time

The compiler is already part of the runtime, so a script can compile code
from a string, if the host allows it:

```eit
import compiler from "eitri:compile"

let f: fn(i64) -> i64 = try compiler.load(source) else e => Error {
    message: "plugin: ${e}",
}
let answer = f(21)
```

- The source is compiled as a module. It must define `pub fn main`, and the
  type of `main` is checked against the **expected type** at the call site,
  the same way enum shorthand works. No generics are needed.
- `compiler.load` returns `CompileError!T`, where `T` is the expected type.
  `CompileError` carries the diagnostics.
- The chunk's top-level code runs once, when it is loaded. A failure there is
  returned as an error, or reported as a fault, like any module
  initialization.
- The chunk can import only modules its caller could import, or fewer, as
  the host decides.
- `"eitri:compile"` exists only if the host enables it.

**Cost:** a normal compilation, which is the fast path Eitri is built for.

## Hot reload

**Status: goal, design incomplete.** The rules below describe what a reload
does to a module. How it interacts with the optimizations promised
elsewhere is not settled yet; see [Open interactions](#open-interactions).

Reloading a module while keeping its live state is a host API:

```
runtime.reload("ui", new_source)
```

| Part of the module | On reload |
|---|---|
| Function with the same name and signature | Its code is replaced |
| Function that is new | Added |
| Function that was removed, or whose signature changed | Rejected, if anything still refers to it; otherwise dropped |
| Module binding with the same name and type | **Keeps its current value**; its initializer doesn't run again |
| Module binding that is new | Initialized |
| Module binding that was removed | Dropped |
| Type definition | Must be unchanged; otherwise the reload is rejected |

- **Running coroutines** finish the frames they are already in on the old
  code. Calls they make afterwards use the new code. Old code stays alive
  until no frame uses it.
- The reload is checked fully before it is applied. A rejected reload changes
  nothing, and the host receives the diagnostics.
- Calls go through a function index, so replacing a function is an update to
  the module's function table. That alone doesn't make reload free: the
  optimizations below can copy a function's code or a value into other
  places.

### Open interactions

Each of these needs a replacement or invalidation rule before hot reload is
decided:

| Optimization | What a reload must do |
|---|---|
| Inlining (such as trivial getters) | Recompile or invalidate every caller that inlined a replaced function, including callers in other modules |
| Generic specialization | Regenerate instantiations of replaced generic functions, or keep old and new copies side by side |
| Folded `pub let` constants | Probably nothing: a binding keeps its value across a reload, so folded copies stay correct. To be confirmed |
| Inline caches | Invalidate caches whose guards depend on replaced code |
| Existing closures | Decide whether a closure created before the reload runs its old code or the new code |
| Frames on old code | Keep old code and its metadata (stack maps, debug tables) alive until the last frame using it returns |

One way to keep this bounded is for a runtime to opt in to reload, and for
cross-module inlining to be disabled in runtimes that do. That costs nothing
in runtimes that never reload.
