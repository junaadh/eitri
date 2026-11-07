# Bindings and functions

## `let` and `var`

```eit
let name = "Ada"          // immutable, type inferred as string
var count = 0             // mutable, type inferred as i64
let limit: i64 = 100      // annotated
```

`let` bindings can't be reassigned. `var` bindings can.

What `let` means for the *value* depends on the kind of type:

| Bound value | `let` forbids | `var` allows |
|---|---|---|
| `struct` | Rebinding **and** mutating any field | Both |
| `class` | Rebinding only; the object itself can still be mutated | Both |
| `[]T`, `map[K]V` | Rebinding only; the contents can still be changed | Both |
| `[N]T` | Rebinding **and** changing elements | Both |

```eit
let v = Vec2 { x: 1, y: 2 }
v.x = 3                   // error: v is a let-bound struct

let p = Player { position: v, health: 10 }
p.health -= 1             // OK: Player is a class
p = other                 // error: p is let-bound
```

Variables are visible only after their declaration.

### Shadowing

A name can be declared again, in the same scope or an inner one. The new
binding hides the old one from that point on:

```eit
let input = app.read_line()          // string
let input = input.trim()             // string, trimmed
let input = i64.parse(input) else _ => 0   // i64
```

The old binding still exists for closures that captured it. Type names can't
be shadowed; see [Lexical structure](lexical-structure.md#predeclared-type-names).

 Variables are local by
default; see [Modules](modules.md#local-by-default).

### Destructuring

Structs destructure by field name. A `name: binding` pair renames:

```eit
let { x, y: py } = pos      // binds x and py
```

Arrays destructure only when they are **fixed-size**, and the number of names
must equal `N`:

```eit
let pair: [2]i64 = [3, 4]
let [a, b] = pair

let list = [3, 4]
let [c, d] = list         // error: list is []i64, not [2]i64
```

## Functions

```eit
fn resize(width: i64, height: i64 = 600) -> void {
    ...
}
```

Every parameter has a type. A parameter may have a default.

### Calling

Arguments can be positional or labeled. Labels are resolved at compile time,
so they cost nothing at run time:

```eit
resize(800)
resize(800, 900)
resize(width: 800, height: 900)
resize(800, height: 900)
```

Labeled arguments may follow positional ones, but not the reverse.

Labeled arguments may come in any order, after the positional ones.
Arguments are always evaluated in the order they are written; the compiler
places them into the right slots, so reordering costs nothing:

```eit
resize(height: 900, width: 800)
```

### Default expressions

A default expression is evaluated **at each call site** that omits the
argument, not once when the function is declared. The same holds for default
field values in struct literals. This is what lets
`caller_location()` record the caller's location:

```eit
fn fail(message: string, location: ?SourceLocation = caller_location()) -> Error {
    return Error { message: message, location: location }
}
```

### Return types and inference

A declared function may omit its return type, and the compiler infers it from
the body. Functions in a recursive group (one that calls itself directly or
through others) must annotate their return types. That keeps inference to a
single pass over the module's call graph; see
[Compiler: module analyses](../implementation/compiler.md#module-analyses).

```eit
fn square(n: i64) { n * n }                  // inferred: i64

fn fact(n: i64) -> i64 {                     // recursive: annotation required
    return if n <= 1 { 1 } else { n * fact(n - 1) }
}
```

A function with no `->` and no value in its body returns `void`.

### Forward references

Top-level function signatures are collected in a preliminary pass, so a
top-level function can call one declared later in the module.

Calling a function early must not read a module binding before it is
initialized. The compiler checks this; see
[Modules](modules.md#initialization-order).

Functions may also be declared inside blocks. Visibility of nested functions
follows the same rule as variables: they are visible after their declaration.
A nested function is a closure, and can use the variables declared before it;
see [Captures](#captures).

## Lambdas

```eit
let double = (n: i64) => n * 2
let positive: fn(i64) -> bool = n => n > 0
let greet = () => {
    app.log("hello")
}
```

A lambda's body is either a single expression or a block. A single parameter
without a type annotation may drop the parentheses.

An **unannotated** parameter needs an expected type to come from somewhere: a
binding annotation, a parameter type, or a function type it is passed into.

```eit
let f = n => n + 1                 // error: no expected type for n
let g: fn(i64) -> i64 = n => n + 1 // OK
items.filter(item => item.ready)   // OK if filter's parameter type is known
```

`try` inside a lambda returns from the lambda, not from the enclosing
function. See [Errors](errors.md#try).

## Captures

Lambdas and nested functions capture **variables**, not snapshots of their
values. A closure sees later changes to a captured variable, and can change it
itself, as Lua upvalues do:

```eit
fn counter() -> fn() -> i64 {
    var n = 0
    return () => {
        n += 1
        return n
    }
}

let next = counter()
next()        // 1
next()        // 2
```

The compiler picks the cheapest capture with these semantics, by looking only
at the enclosing function:

| Captured | How | Cost |
|---|---|---|
| `let` binding | Copied into the closure | The copy |
| `var` that nothing assigns after the closure is created | Copied; no one can tell the difference | The copy |
| `var` assigned after capture, inside or outside the closure | **Shared variable** | One shared cell per variable |
| Module binding | Not captured; accessed directly | None |

Further rules:

- **Fresh bindings per loop iteration.** A closure created inside
  `for i in 0..n` sees its own `i`:

  ```eit
  var handlers: []fn() -> i64 = []
  for i in 0..3 {
      handlers.push(() => i)
  }
  // handlers return 0, 1, 2
  ```

- **No special cases for value and reference types.** Capturing a `var`
  struct shares the *variable*, and the struct is still a value. Capturing a
  class instance or a collection copies the reference, so the object is
  shared, as with any assignment.
- **`self` in methods** follows the type:
  - In a class method, `self` is a reference and is captured as one.
  - In a struct method, `self` is captured by copy.
  - In a `mutating` struct method, closures also capture a copy of `self`,
    and can't assign to it or its fields. Otherwise a closure that outlives
    the call could write into the caller's variable.
- **Allocation.** Creating a closure that captures something allocates one
  closure object. A function value that captures nothing (a top-level
  function, a static function, or a lambda with no captures) allocates
  nothing.
- **Narrowing.** Captured `var`s are never [narrowed](optionals.md#narrowing),
  since a closure could change them between the check and the use.
- **Tasks.** Closures run by different tasks can share a captured variable.
  Tasks never run in parallel, so each access is atomic, but a value read
  before an `await` may have changed after it.

## Async functions

A function declared `async` may suspend the current coroutine. Every call to
it is written with `await`, and `await` is only allowed inside another `async`
function or lambda:

```eit
async fn blink(light: Light) -> void {
    light.on()
    await app.sleep(0.5)
    light.off()
}
```

A lambda is `async` when its expected type is an `async` function type, so
`app.on_start(() => { await blink(lamp) })` needs no annotation. Without an
expected type, write `async () => ...`.

Module top-level code is not async and can't use `await`, but any code,
including top-level code, can start a task with `spawn`:

```eit
let blinker = spawn blink(lamp)     // blinker: task void
blinker.cancel()
```

The full rules are in [Coroutines and async](../design/coroutines.md).

## Assignment

```eit
count = 10
count += 1
total &+= delta       // wrapping add
grid[i] = 0
settings["theme"] = "dark"
player.health -= 5
```

The left side must be an assignable place:

- a `var` binding,
- a field of an assignable struct (one reached through `var`),
- a field of a class reference (through `let` or `var`),
- an array or map element.

Assignment is a statement, not an expression. It produces no value.
