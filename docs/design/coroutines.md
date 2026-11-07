# Design note: coroutines, async, and tasks

**Status:** decided. Generators are explicitly deferred. Switching costs are an
implementation measurement, tracked in
[Performance goals](../implementation/performance.md#coroutine-costs).

## Summary

Eitri uses **explicit `async`/`await` syntax on stackful coroutines, with
the host as the scheduler.**

- `async` in a function's signature means the function may suspend.
- Every call to an `async` function is written with `await`.
- An awaited call is an ordinary VM call on the current coroutine's stack.
  Suspending preserves every script frame in that coroutine.
- `spawn` starts an independent coroutine, called a **task**, and returns a
  `task T` handle. `await` on a handle joins the task.
- Eitri has no event loop of its own. Only host `async` functions suspend
  waiting on the outside world, and the host decides when they resume.
  Coroutines woken by other script code go through one FIFO
  [ready queue](#the-ready-queue), drained before control returns to the
  host.
- A coroutine can never suspend across a host frame, and the type system
  enforces that rule.

```eit
// Host-provided declaration
async fn sleep(seconds: f64) -> void

async fn blink(light: Light) -> void {
    while true {
        light.on()
        await app.sleep(0.5)
        light.off()
        await app.sleep(0.5)
    }
}

let blinker = spawn blink(lamp)       // runs until its first sleep

app.on_key("q", () => {
    blinker.cancel()
})
```

## Async functions

```eit
fn(i64) -> void          // cannot suspend
async fn(i64) -> void    // may suspend
```

1. `async` is part of a function's type, and it is written in its declaration.
2. Calling an `async` function requires `await`.
3. `await` is allowed only inside an `async` function or `async` lambda.
4. An `async` function calls ordinary functions normally.
5. Declaring `async` permits suspension. The body is not required to actually
   suspend.

Because `async` is in the signature, the preliminary signature pass knows
everything callers need. Forward references and recursion need nothing extra.

### Conversion from sync to async

A plain function converts implicitly to the matching `async` function type:

```
fn(A) -> R   →   async fn(A) -> R
```

Execution is stackful, so a sync call and an awaited call use the same call
sequence, and the conversion costs nothing at run time. This is the third
implicit conversion, alongside `T → ?T` and `T → E!T`. The reverse conversion
does not exist.

### Lambdas

Expected types flow into lambdas, so a lambda becomes `async` when its expected
type is an `async` function type:

```eit
app.on_start(() => {
    await run(lamp)            // OK: on_start expects async fn() -> void
})
```

`async () => ...` is needed only when there is no expected type.

### `await` and `try`

`await` is a prefix operator at the same level as `try`, and it takes a single
postfix chain. When both apply, `try` comes first:

```eit
async fn load_remote(url: string) -> LoadError!Config {
    let text = try await app.fetch(url) else _ => .NotFound(path: url)
    return parse_config(text)
}

let title = (await current_window()).title     // parentheses end the chain
```

### Where `await` is allowed

| Context | `await` allowed? | Why |
|---|---|---|
| `async fn` body or `async` lambda | Yes | |
| Plain `fn` body | No | The signature promises it cannot suspend |
| `defer` block | No | Cleanup must not suspend, matching the ban on `try` |
| Module top-level code | No | Setup scripts stay on the fast path. Use `spawn`, or have the host call an exported `async fn` |

## Tasks

### `spawn`

```eit
let t = spawn blink(lamp)          // t: task void
let c = spawn load_remote(url)     // c: task <LoadError!Config>
```

- The operand is a call to any function, `async` or plain. Its arguments are
  evaluated immediately. A plain function is an `async` function that never
  suspends, as the implicit conversion already says, so spawning one runs it
  to completion inside `spawn`. That is how code is isolated for
  [fault containment](#faults-in-tasks).
- The result is a `task T`, where `T` is the function's return type.
- **Spawning is eager.** The new coroutine runs on its own stack until it
  first suspends or finishes, and then control returns to the statement after
  `spawn`. Background work therefore starts immediately, even if nothing ever
  joins it. (Lazy start on first `await` would also be possible without a
  scheduler, but a task nobody joins would then never run.)
- `spawn` is allowed anywhere, including plain functions, `defer` blocks, and
  module top-level code. The parent does not suspend, and the child's own
  stack has no host frames, so the host-frame rule still holds.

### The `task T` type

`task T` is a garbage-collected handle. `task` is a contextual word at the
start of a type, like `map`. Like other prefix constructors, it binds tighter
than `!`, so a task returning a result is written `task <E!T>`.

| Member | Type | Behavior |
|---|---|---|
| `await t` | `Stopped!T` | Joins the task, suspending until it finishes. See below |
| `t.result()` | `?<Stopped!T>` | The outcome without suspending: `null` while the task is unfinished. Usable in plain functions and top-level code |
| `t.cancel()` | `void` | Requests cancellation. See [Cancellation](#cancellation) |
| `t.is_done()` | `bool` | Whether the task has reached a terminal state |

`task T` is not formattable.

### Terminal states and joining

A task ends in exactly one terminal state. Joining it with `await t` behaves
according to that state:

| Terminal state | `await t` |
|---|---|
| **Completed** with a value `v` | Produces `.Ok(v)` |
| **Cancelled** | Produces `.Err(.Cancelled)` |
| **Faulted** | Produces `.Err(.Faulted(fault))` |

`Stopped` is a compiler-known enum:

```eit
enum Stopped {
    Cancelled
    Faulted(fault: Fault)
}
```

`Fault` carries the fault's message and source trace, and it is formattable.

- Joining a task that has already finished returns immediately, without
  suspending.
- Any number of coroutines can await the same task. When the task finishes,
  its waiters resume in **FIFO order**, in the order they started waiting,
  within the same host call that finished the task.
- A faulted task keeps its fault and trace, so a waiter is never left
  stranded. Joining it later still produces `.Err(.Faulted(fault))`.

#### Tasks whose result is a result

The two error layers **nest**. There is no special rule:

```eit
let c = spawn load_remote(url)       // task <LoadError!Config>

match await c {                      // Stopped!<LoadError!Config>
    .Ok(.Ok(config)) => apply(config),
    .Ok(.Err(error)) => app.log("load failed: ${error}"),
    .Err(.Faulted(fault)) => app.log("crashed: ${fault}"),
    .Err(.Cancelled) => app.log("cancelled"),
}

let maybe = try? await c             // ?<LoadError!Config>
```

Nesting follows from existing rules. The error position of `!` accepts only a
named type, and Eitri has no anonymous union types to merge `Stopped` and
`LoadError` into.

### Faults in tasks

A fault terminates the **coroutine it happens in**:

1. That coroutine's pending `defer` blocks run, innermost outward.
2. It enters the Faulted state and keeps the fault and its trace.
3. Its waiters resume in FIFO order, and each receives
   `.Err(.Faulted(fault))`.

A fault in a spawned child doesn't terminate its parent. This holds even when
the child faults before its first suspension, while it is running inside the
parent's `spawn`. In that case `spawn` returns a handle that is already
faulted. The rule makes the outcome independent of *when* the child first
suspends.

**The task is the containment boundary**, as processes are in Erlang. A fault
is still unrecoverable inside the task where it happens, but other code sees
it as data. This is how a script contains a buggy subsystem, which Lua scripts
do with `pcall`:

```eit
let t = spawn run_plugin(plugin)        // a plain function: runs to completion here
match t.result() {
    .Ok(_) => {},
    .Err(.Faulted(fault)) => app.log("plugin crashed: ${fault}"),
    .Err(.Cancelled) => {},
    null => {},                          // only if run_plugin suspended
}
```

Containment costs one coroutine, and only where it is used.

The host learns about every fault, including those a script also observes.
See [Host API](#host-api).

## Cancellation

Cancellation takes effect **only at `await` points**. Script code is never
interrupted between two ordinary statements.

| Task state when `cancel()` is called | Effect |
|---|---|
| **Suspended** | Takes effect immediately. Pending `defer` blocks run innermost outward, inside the call to `cancel()`. The task becomes Cancelled |
| **Active** (anywhere on the current call chain, including the caller itself) | Records a request. The task's **next `await`** unwinds instead of continuing, whether or not it would have suspended |
| **Finished** | No effect |

- **Completion wins.** If an active task with a pending request returns
  without executing another `await`, it Completes normally. Its waiters
  receive `.Ok(v)`.
- **Cleanup can't suspend.** `await` is not allowed in `defer`, so cleanup
  always runs to completion.
- **Waiters observe cancellation; they are not cancelled.** A coroutine
  awaiting a cancelled task receives `.Err(.Cancelled)` and can do fallback
  work. That includes a task cancelled *while* the waiter was suspended on
  it.
- **Waiting doesn't block cancellation.** Cancelling a task that is suspended
  joining another task removes it from that task's waiters.
- **Host operations are notified.** If the cancelled task was suspended in a
  host `async` function, the host's `on_cancel` hook for that operation runs,
  so it can drop the timer or I/O. See [Host API](#host-api).
- **A fault inside a `defer` during cancellation** makes the task Faulted
  instead of Cancelled, and the fault is reported to whoever requested the
  cancellation.

There is no structured concurrency by default: a parent finishing doesn't
cancel its children. Tracking children would be a recurring cost on every
task. Scripts that want it cancel their tasks in a `defer`:

```eit
async fn session() -> void {
    let heartbeat = spawn beat()
    defer { heartbeat.cancel() }
    await run_session()
}
```

## The host is the scheduler

Only host `async` functions actually suspend. Script `async` functions
suspend only by awaiting, directly or transitively, a host operation or an
unfinished task. Every suspended coroutine is therefore waiting on exactly one
of:

- **a host operation**, whose suspension token the host holds, or
- **a task**, which holds the coroutine in its waiter list.

A host operation is resumed by the host. Everything woken by script code (a
finishing task's waiters, a channel's receivers) goes through the ready
queue.

### The ready queue

The VM keeps one **ready queue** per runtime: coroutines that can continue
and are waiting for their turn. It is a small scheduler, and its rules are
part of the embedding contract:

- **Order:** FIFO, in the order coroutines became ready. A finishing task
  enqueues its waiters in the order they started waiting; a `send` enqueues
  the receiver it wakes.
- **When it drains:** after the current coroutine suspends or finishes, and
  before the outermost host call returns. Each start, resume, or cancel call
  from the host drains it.
- **Budget:** draining uses the budget of the host call doing it. If the
  budget runs out, the running coroutine faults (with cleanup as described in
  [Faults](../language/errors.md#faults)), and coroutines still in the queue
  **stay queued**. The report says how many remain, and the host drains them
  with `runtime.drain()` when it chooses.
- **Cost:** the queue is empty unless script code woke something. Code that
  never joins tasks or uses channels never touches it.

### Host API

The API is described language-neutrally here; the embedding chapter covers
binding details.

**Starting tasks.** When the host invokes a script callback of `async` type,
such as the lambda passed to `app.on_start`, the runtime creates a task to run
it.

**Suspending.** When script awaits a host `async` function, the host function
receives a **suspension token** and either:

- **completes immediately** by returning a value, with no suspension or
  switch (the fast path, for example `sleep(0)` or a cache hit), or
- **suspends** by keeping the token, optionally registering an `on_cancel`
  hook with it.

**Resuming and cancelling.** Later, the host calls one of:

| Call | Effect |
|---|---|
| `resume(token, value)` | Resumes the suspended coroutine. `value` has the function's declared return type |
| `cancel(token)` | Cancels the task that is suspended on this operation |
| `cancel(task)` | Cancels a task the host holds a handle to |

Errors are values: a host function declared `-> E!T` resumes with an `.Err`.
There is no separate "resume with error", and **the host cannot inject a
fault**. If an operation can fail in a way scripts should handle, its
signature says so.

**Reports.** Starting, resuming, and cancelling run script code until control
returns to the host, possibly finishing several tasks along the way (for
example, a resumed task completes and its waiters run). Each call returns a
report:

- **the primary task** this call was about, identified by its task ID, and
  its outcome: `Suspended`, `Completed(value)`, `Cancelled`, or
  `Faulted(trace)`, and
- **every other task** that reached a terminal state during the call, with
  its ID and outcome.

No fault is lost: every faulted task appears in exactly one report.

**Tokens.**

- A token is one-shot, and it carries a generation number. Using it twice,
  or after its task was cancelled, returns an error to the host. This is never
  a script fault.
- A held token keeps its coroutine alive.
- A token dropped without `resume` or `cancel` leaves its coroutine
  unreachable. It will be collected without running its `defer` blocks.
  Debug builds report this.

### Reentrancy

A host function that script called may start, resume, or cancel coroutines in
the same runtime.

- A coroutine is **active** while it is anywhere on the current call chain.
  Only suspended coroutines can be resumed. Resuming an active coroutine is a
  host API error.
- Cancelling an active coroutine from the host records a request, exactly as
  in script.
- Nested runs share the **outermost host call's execution budget**, so
  reentrancy can't be used to get around the budget.

## No suspension across host frames

If script calls a host function that calls back into script, the callback
must not suspend across that host function. Otherwise, the host's native
stack would have to be suspended too.

Eitri enforces this through types. A host function that takes a callback
declares a **plain** function type for it:

```eit
// host: fn sort_by(items: []Item, key: fn(Item) -> i64) -> []Item

let sorted = app.sort_by(items, async (item) => {   // compile error:
    await app.sleep(1.0)                           // key is not async
    item.rank
})
```

A sync callback that needs background work can still `spawn`, because the
child runs on its own stack.

## Memory and lifetime

The garbage collector only releases memory. It never runs `defer` blocks or
any other script code.

What keeps a coroutine alive:

- the host holding its suspension token or task handle,
- script references to its `task` handle,
- for a waiting coroutine: the task it awaits holds it as a waiter, and it in
  turn keeps that task alive. Both directions of the dependency are
  references.

A suspended coroutine that becomes unreachable, for example after a dropped
token or two tasks awaiting each other, **never exits**. Its pending `defer`
blocks never run. This is a deliberate limit on `defer`'s "every exit"
guarantee. Debug builds report such coroutines when they are collected.

The host must complete or cancel outstanding coroutines before destroying the
runtime.

## Why this design

The deciding test is the founding principle: *resolve what is known, specialize
what is learned, execute directly, and make every recurring cost earn its
place.*

### Why stackful

Languages compiled to native code (Rust, C#) choose stackless async because
their frames live on the native stack, which is expensive to save. Eitri's
frames already live in VM memory, so keeping them in place when a coroutine
suspends is natural:

- Calls that never suspend pay nothing extra.
- An `await` call is an ordinary call. It doesn't create a future or set up a
  poll chain.
- The sync-to-async conversion is free.

Switching coroutines still has bookkeeping: the current coroutine, stack
bounds, GC roots, the pending `defer` chain, and the host-frame barrier.
Stackless execution doesn't inherently need a per-call allocation either. The
real costs are measured once the VM exists, and the semantics above don't
depend on the outcome.

### Why explicit `async` rather than inferred suspension

An earlier draft inferred suspension from a function's body. That fails for
callers: a caller would need information from the callee's body, which the
signature pass can't provide, so forward references and recursion break.
`async` in the signature fixes that, and it makes callback types say whether
they may suspend.

### Why explicit `await` at call sites

`await` marks where control can leave a function, just as `try` marks where an
error can leave it. It is also the only place cancellation can take effect. It
has no runtime cost and tells the reader something real: state reachable from
elsewhere (class fields, captured variables) may have changed across it.

### Why `Stopped!T` rather than cascading cancellation or faults

A shared task may have several consumers, and some of them have fallback
work. Making cancellation and faults observable values keeps that choice with
each consumer. Checking a flag before awaiting would not cover a task that
stops while the consumer is suspended. Faults in particular need to be
observable for Lua parity: containing a failing subsystem is what Lua scripts
use `pcall` for.

### Why not transparent suspension (Go's model)

In Go, any function can block its goroutine. Eitri could copy that while
leaving scheduling to the host, but then a signature would no longer say
whether a function can suspend. Protecting synchronous host callbacks would
need effect inference, restricted APIs, or a runtime check, and the last turns
a type error into a fault.

### Why not a built-in scheduler

A scheduler inside the VM would duplicate the host's loop and add a recurring
cost to every script, including the one-shot setup scripts that are the
common case.

## Native tier

`async` in the type tells a future native tier exactly which functions can
suspend. It can keep those frames in VM memory and compile everything else
with ordinary native frames.

## Deferred: generators

Value-producing generators (`yield`) are not in this version. Iterating them
with `for` needs an iteration protocol; [generics and interfaces](generics.md#what-this-enables)
now make `Iterator<T>` possible. Generators would reuse the same stackful
machinery, so nothing here blocks them. `yield` is reserved as a keyword now.
