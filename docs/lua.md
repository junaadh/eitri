# Eitri and Lua

Eitri is a modern redefinition of Lua. It keeps Lua's role as a small,
embeddable language that a host fully controls, and anything a Lua script can
achieve, an Eitri script can achieve too.

> **Parity rule:** every *outcome* a Lua script can achieve must be
> achievable in Eitri. The mechanism may differ. Eitri resolves names and
> shapes at compile time where Lua uses tables at run time, so the Eitri route
> is often a different construct with a known cost.

This page lists what Lua scripts do, and how Eitri gets the same result.

Legend:

- ✅ **Same:** the same construct, or one that is directly equivalent.
- 🔁 **Different route:** the same outcome through a different construct.
- 📚 **Library:** needs a standard library or host module function, with no
  language change.
- ⚠️ **Gap:** the outcome can't be achieved yet. Tracked in
  [Open questions](open-questions.md#lua-parity).

## Globals and modules

| In Lua you… | In Eitri you… | |
|---|---|---|
| Load a module once with `require` | `import`; one instance per runtime | ✅ |
| Return a table of exports | Mark items `pub` | 🔁 |
| Share state across scripts with a global | Keep it in a module and import it | 🔁 |
| Change another module's state (`config.volume = 90`) | Export a class instance (`pub let config = Config {…}`) and assign its fields, or call an exported setter | 🔁 |
| Replace a function at run time (monkey patching) | Call through a function-typed field: `pub let hooks = Hooks { format: default_format }`, then `hooks.format = mine` | 🔁 |
| Look up a value by name string (`_G[name]`) | A `map[string]T`, or a `map[string]dyn` for values of mixed types | 🔁 |
| Sandbox a chunk (`_ENV`, `setfenv`) | The host controls what each script can import | 🔁 |
| Write mutually dependent modules | Move the shared part to a third module, or pass functions in | 🔁 (cycles themselves are open) |
| Hot reload code | The host recompiles in a fresh runtime | 🔁 |
| Hot reload code **and keep live state** | The host's `runtime.reload`: functions are replaced, module state is kept. See [Hot reload](design/runtime-features.md#hot-reload) | 🔁 |

## Tables and data

| In Lua you… | In Eitri you… | |
|---|---|---|
| Use a table as a list | `[]T` (shared by reference, like a Lua table), `[N]T` | 🔁 |
| Use a table as a dictionary | `map[K]V` | 🔁 |
| Use a table as a record or object | `struct`, `class` | 🔁 |
| Compare tables with `==` (identity) | `==` on classes, arrays, and maps is identity; `equals()` compares content | ✅ |
| Use any value as a table key | Any type with `==` can be a map key, including objects and `null` | ✅ |
| Iterate with `pairs` / `ipairs` | `for k, v in m` (insertion order) / `for i, x in a` | ✅ |
| Get a length with `#t` | `t.len()` | 🔁 |
| Leave `nil` holes in a list | `[]?T` | 🔁 |
| Hold mixed-type or JSON-like data | `dyn`, with `d.field`, `d[k]`, and `json.parse`. See [The dynamic type](design/dynamic.md). A typed alternative is a recursive enum, below | ✅ |
| Give missing keys a default (`__index`) | `m.get(k)` with `??`, or a method | 🔁 |
| Intercept reads and writes (proxy via `__index`/`__newindex`) | A class with methods instead of raw fields | 🔁 |
| Add operators to a type (`__add`, `__eq`, `__lt`) | Named methods: `a.add(b)` | 🔁 |
| Make an object callable (`__call`) | A function-typed field, or a method | 🔁 |
| Customize printing (`__tostring`) | `describe() -> string` | ✅ |
| Build OOP with shared behavior (metatable inheritance) | Interfaces with default methods, plus composition | 🔁 |
| Treat different kinds of objects alike (duck typing) | Interfaces: `[]any Shape` for mixed lists, or generic functions with a constraint | 🔁 |
| Hold values without keeping them alive (`__mode` weak tables) | `weak T`, and weak-key maps `map[weak K]V`. See [Weak references](design/runtime-features.md#weak-references) | 🔁 |
| Run cleanup when an object is collected (`__gc`) | `runtime.on_collect(obj, cleanup)`, run at a safe point after collection. See [Cleanup on collection](design/runtime-features.md#cleanup-on-collection) | 🔁 |

Mixed-type data with a user-defined enum:

```eit
enum Json {
    Null
    Bool(value: bool)
    Number(value: f64)
    Text(value: string)
    List(items: []Json)
    Object(fields: map[string]Json)
}
```

Arrays and maps are references, so this needs no `@indirect`. It trades the
convenience of `dyn` for full static checking.

## Functions

| In Lua you… | In Eitri you… | |
|---|---|---|
| Pass functions around and close over variables | Functions and lambdas | ✅ |
| Pass an options table (`opts.x or 5`) | Default and named arguments | ✅ |
| Return several values | Return a struct and destructure it: `let { x, y } = pos()` | 🔁 |
| Take a variable number of arguments | Take an array: `fn sum(values: []i64)` | 🔁 |
| Format with mixed arguments (`print(a, 1, true)`) | Interpolation: `"${a} 1 true"` | 🔁 |
| Compile and run code from a string (`load`, `dofile`) | `compiler.load(source)`, checked against the expected function type. See [Compiling code at run time](design/runtime-features.md#compiling-code-at-run-time) | 🔁 |
| Ship precompiled chunks (`string.dump`) | Not needed for speed: compilation is the fast path | 🔁 |

## Errors

| In Lua you… | In Eitri you… | |
|---|---|---|
| Signal an expected failure (`error` + `pcall`) | Return `E!T`; handle it with `try` or `match` | 🔁 |
| Fall back to a default when a call fails (`pcall` + `or`) | `f() else e => fallback` | 🔁 |
| Use `x or default` | `x ?? default` | ✅ |
| Use `a and b or c` | `if a { b } else { c }` | ✅ |
| Get a traceback | Fault traces, `caller_location()` | ✅ |
| Survive a **bug** in a subsystem and carry on (`pcall` around buggy code) | `spawn` it, then read `.Err(.Faulted(fault))` from `await t` or `t.result()`. See [Faults in tasks](design/coroutines.md#faults-in-tasks) | 🔁 |

## Coroutines

| In Lua you… | In Eitri you… | |
|---|---|---|
| Suspend inside nested calls | `async`/`await`, stackful | ✅ |
| Start a coroutine (`create` + `resume`) | `spawn` | 🔁 |
| Stop a coroutine (`coroutine.close`) | `t.cancel()` | ✅ |
| Check a coroutine's state (`coroutine.status`) | `t.is_done()` | 🔁 |
| Write a lazy sequence or iterator (`coroutine.wrap`) | A closure that returns `?T`: `while let x = next() { ... }` | 🔁 |
| Wait for the host (timers, frames, I/O) | `await` a host `async` function | ✅ |
| Wake one script task from another script task (signals, events, a scheduler written in script) | `chan T`: `send` from anywhere, `await c.receive()` in a task. See [Channels](design/runtime-features.md#channels) | 🔁 |

## Numbers and strings

| In Lua you… | In Eitri you… | |
|---|---|---|
| Use 64-bit integers and floats | `i64`, `f64`, plus `i8`, `u8`, `u64`, `f32` for compact or unsigned data | ✅ |
| Use bitwise operators | `&`, `\|`, `^`, `~`, `<<`, `>>` | ✅ |
| Rely on wrapping overflow | `&+`, `&-`, `&*` | 🔁 |
| Mix integers and floats (`1 + 2.5`) | Convert explicitly: `f64(n) + 2.5` | 🔁 |
| Floor divide (`//`), floor modulo | `floor_div(a, b)`, `floor_mod(a, b)` | 📚 |
| Raise to a power (`^`) | A library function | 📚 |
| Concatenate (`..`) | `a + b`, or interpolation | 🔁 |
| Build a string from pieces (`table.concat`) | `parts.join(sep)` | 🔁 |
| Parse numbers from strings (`"10" + 1`, `tonumber`) | `i64.parse(s)`, returning `ParseError!i64` | 🔁 |
| `string.format` | Interpolation | 🔁 |
| `string.byte`, `string.char`, byte access | `s.byte_at(i)` and `b[i]` give `u8`; `[]u8` is a mutable buffer; `char` values | 🔁 |
| Lua patterns (`find`, `match`, `gsub`, `gmatch`) | A pattern library | 📚 |
| `utf8` library | Strings are UTF-8: `for c in s`, `char_count()`, `string.from_utf8` | 🔁 |

## Control flow

| In Lua you… | In Eitri you… | |
|---|---|---|
| `if`, `while`, `for`, `break` | The same, plus `if` and `match` as expressions | ✅ |
| `repeat … until cond` | `while true { …; if cond { break } }` | 🔁 |
| `goto continue` | `continue` | ✅ |
| Other `goto` uses (leaving nested loops) | Labeled loops: `break outer`, `continue outer` | 🔁 |

## Embedding and runtime

| In Lua you… | In Eitri you… | |
|---|---|---|
| Use the C API | Typed host signatures, frame-based ABI | ✅ |
| Use userdata | Opaque handles, `@extern` structs | ✅ |
| Plug in an allocator | Custom allocators, memory limits | ✅ |
| Count instructions with `debug.sethook` | Built-in execution budgets | ✅ |
| Run separate states | Isolated runtimes | ✅ |
| Control the GC (`collectgarbage`) | The host API | 🔁 |
| Inspect frames and locals (`debug` library) | The host, through the debug metadata | 🔁 |
| Use `io`, `os`, `math`, `table` | Host modules and the standard library | 📚 |

## Summary

Every outcome a Lua script can achieve now has an Eitri route. What remains
is a standard library to write (the 📚 rows), and convenience features that
would make Lua code easier to port, such as tuples, varargs, and operator
overloading. Those are tracked in
[Open questions](open-questions.md#lua-parity). None of them is needed for
parity.
