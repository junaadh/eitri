# Design note: generics and interfaces

**Status:** decided. Explicit type arguments in expressions and extensions
are deferred.

## Generic declarations

Functions, structs, classes, enums, interfaces, and type aliases can take type
parameters:

```eit
fn max<T: Ordered>(a: T, b: T) -> T {
    return if a > b { a } else { b }
}

struct Pair<A, B> {
    first: A
    second: B
}

class Stack<T> {
    items: []T = []

    static fn new() -> Stack<T> {
        return Stack {}
    }

    fn push(x: T) -> void {
        items.push(x)
    }
}

enum Tree<T> {
    Leaf
    @indirect Node(left: Tree<T>, value: T, right: Tree<T>)
}

type Grid<T> = [][]T
```

### Type arguments are inferred

Type arguments use angle brackets in **type positions** only, where the type
parser already handles `<…>`. In expressions, they are inferred from the
arguments and from the expected type, the same inference that powers enum
shorthand:

```eit
let m = max(3, 7)                          // T = i64, from the arguments
let p = Pair { first: 1, second: "x" }     // Pair<i64, string>
let s: Stack<i64> = Stack.new()            // T = i64, from the expected type
```

Explicit type arguments in expressions (`f<T>(x)`) are deferred. They would
reintroduce the ambiguity between `<` as a type bracket and as an operator.
When inference has nothing to go on, annotate the binding instead.

### Checked once, where declared

A generic body is type-checked **once, against its constraints**, at its
declaration. Inside `max`, `a > b` is valid because `T: Ordered` says so, not
because some caller happens to pass integers. Errors appear at the definition,
not at each use, as in Rust and Swift and unlike C++ templates.

This keeps two properties:

- A script is fully checked before it runs.
- Checking cost doesn't multiply with the number of instantiations.

## Constraints

A type parameter can be constrained by interfaces and built-in constraints,
combined with `+`:

```eit
fn index_by<K: Eq, V>(items: []V, key: fn(V) -> K) -> map[K]V { ... }
fn largest<T: Ordered + Formattable>(xs: []T) -> string { ... }
```

### Built-in constraints

These are satisfied automatically by the language's existing rules. No type
declares them.

| Constraint | Satisfied by | Allows |
|---|---|---|
| `Eq` | Every type that supports [`==`](../language/expressions.md#equality-and-ordering) | `==`, `!=`, use as a map key |
| `Ordered` | Numbers, `char`, `string` | `<`, `<=`, `>`, `>=` |
| `Numeric` | Integer and float types | `+`, `-`, `*`, `/` |
| `Formattable` | Every type that can be [interpolated](../language/strings.md#what-can-be-formatted) | `"${x}"` |

## Interfaces

```eit
interface Shape {
    fn area() -> f32

    fn describe() -> string {               // default body
        return "shape with area ${area()}"
    }
}

struct Circle: Shape {
    radius: f32

    fn area() -> f32 {
        return 3.14159 * radius * radius
    }
}
```

- **Conformance is declared explicitly**, after the type's name. The compiler
  checks it once, at the declaration, with no search for matching methods.
- A conforming type must provide every member without a default body. It may
  replace a default.
- Interfaces can declare `static fn`, `async fn`, and `mutating fn`
  requirements, and can refer to the conforming type as `Self`:

  ```eit
  interface Parse {
      static fn parse(text: string) -> ParseError!Self
  }
  ```

- Interfaces can be generic: `interface Iterator<T> { mutating fn next() -> ?T }`.
- Default bodies plus composition cover what Lua scripts do with metatable
  inheritance.

## Two ways to use an interface

| Form | Dispatch | Run-time cost |
|---|---|---|
| A constraint: `fn total<T: Shape>(xs: []T)` | Static: the code is specialized for each `T` | None |
| A value: `[]any Shape` | Dynamic: a pointer to the value plus a table of its methods | One indirect call per method call. Storing a struct in an `any` boxes it, which is one allocation |

```eit
fn total_area<T: Shape>(shapes: []T) -> f32 { ... }   // all the same shape type

let mixed: []any Shape = [Circle { radius: 1 }, Square { side: 2 }]
for s in mixed {
    app.log(s.describe())                              // dynamic dispatch
}
```

`any` marks the dynamic case, as in Swift. A heterogeneous list is possible,
and its type shows that it pays for dynamic dispatch.

- A value converts implicitly from `T` to `any I` when `T` conforms to `I`.
- `any I` supports `==` only if `I` requires `Eq`. Then values of different
  concrete types are never equal.
- Static members of an interface can't be called through `any I`, since
  there is no concrete type to call them on.

## Implementation: specialization

The compiler generates a copy of the generic code for each set of type
arguments the script actually uses.

- Generated code is as fast as hand-written code: no boxing, no dictionary
  lookups, and scalars stay untagged.
- Combinations that are never used cost nothing, not even compile time.

Specialization multiplies compile time and code size, so it has limits:

- **No polymorphic recursion.** Inside a recursive group, generic calls
  must pass the group's own type parameters unchanged. A call such as
  `f<[]T>` from within `f<T>` would need an endless series of copies, so it
  is a compile error. This is checked on the call graph's recursive groups,
  which the compiler already computes. The ban follows from instantiating
  everything before running; the .NET CLR supports polymorphic recursion by
  instantiating lazily at run time, so it could be lifted later. See
  [R27](../implementation/vm-research.md#r27-specialization-versus-shared-generic-code).
- **An instantiation cap.** Each compilation has a host-configurable limit on
  the number of instantiations, and on how deeply type arguments may nest.
  Exceeding it is a compile error that names the generic responsible.
- **Large aggregates use loops.** Generated equality, hashing, and copying
  are straight-line code only for small types. Above a size threshold, such
  as a `[256]u8` or a struct with dozens of fields, they become a loop or a
  shared helper, so code size stays proportional to the program, not to its
  data.

**Experimental:** instantiations whose types have the same memory shape and
identical behavior could share one copy, as in Go's GC-shape stenciling and
the .NET CLR. That is an optimization to try once measurements exist, not a
commitment, and the evidence suggests small gains: merging duplicate
instantiations in Rust didn't reduce compile time, and sharing through method
dictionaries in Go made calls up to about 42% slower. See
[R27](../implementation/vm-research.md#r27-specialization-versus-shared-generic-code).

**Rejected: erasure**, compiling one copy for every type argument. It would
require a uniform representation, which means boxing scalars that Eitri
otherwise keeps untagged. That is a hidden cost on every generic operation.

## What this enables

- **An iteration protocol.** With `Iterator<T>`, `for x in it` can work on
  any conforming type. That is the piece generators were waiting for.
- **A standard library in Eitri.** Built-in methods such as `push`, `join`,
  and `contains` can become ordinary generic functions and methods.

## Not included

- **Variance:** generic types are invariant. A `[]Circle` is not a
  `[]any Shape`.
- **Specialization and overloading:** one generic definition serves all type
  arguments.
- **Extensions from other modules:** `extend` is limited to the module that
  declares the type or the interface; see
  [Extensions](../language/structs-classes-enums.md#extensions).
- **Explicit type arguments in expressions**, as above.
