# Types

## Built-in types

| Type | Meaning |
|---|---|
| `i8`, `i64` | Signed integers, 8 and 64 bits |
| `u8`, `u64` | Unsigned integers, 8 and 64 bits |
| `f32`, `f64` | IEEE floats, 32 and 64 bits |
| `bool` | `true` or `false` |
| `char` | One Unicode scalar value |
| `string` | Immutable, valid UTF-8 text. See [Strings](strings.md) |
| `bytes` | Immutable raw byte data |
| `void` | The no-value return type |
| `dyn` | A value of any type, tagged with its type at run time. See [The dynamic type](../design/dynamic.md) |

### The scalar set

There are exactly seven numeric and boolean scalars: `i8`, `i64`, `u8`,
`u64`, `f32`, `f64`, and `bool`. Every width is written in the type's name,
and none depends on the target.

- **`i64` and `f64` are the defaults.** An integer literal with no expected
  type is an `i64`, and a float literal is an `f64`. There are no `int` or
  `float` aliases.
- **`i8` and `u8` are for compact storage:** bytes, flags, and packed arrays.
  `u8` is the element type of byte buffers (`[]u8`).
- **`u64`** covers unsigned values, hashes, and bit patterns.
- **`f32`** halves the storage of floating-point data.
- There is no native-width integer. The type checker must know every width
  before the script runs, because it rejects literals that don't fit and
  decides where arithmetic overflows. A width that followed the target would
  make the same script compile or overflow differently on different hosts.

Narrow types describe meaning, not slot size. VM slot width is chosen by
benchmark; see [Virtual machine](../implementation/vm.md#registers-and-frames).

## Type constructors

| Syntax | Meaning |
|---|---|
| `?T` | `T` or `null` |
| `[]T` | Growable array of `T` |
| `[N]T` | Exactly `N` inline values of `T` |
| `map[K]V` | Map from `K` to `V` |
| `E!T` | Either an error `E` or a success `T` |
| `fn(A, B) -> R` | Function type. The return type is always spelled out |
| `async fn(A, B) -> R` | Function that may suspend. See [Coroutines](../design/coroutines.md) |
| `task T` | Handle to a spawned task that produces `T`. See [Tasks](../design/coroutines.md#tasks) |
| `weak T` | A reference to `T` that doesn't keep it alive. See [Weak references](../design/runtime-features.md#weak-references) |
| `chan T` | A FIFO channel between tasks. See [Channels](../design/runtime-features.md#channels) |
| `<T>` | Grouping |

User types are introduced by [`struct`, `class`, and `enum`](structs-classes-enums.md),
and names can be qualified by module: `errors.LoadError`.

### Generic types and interfaces

| Syntax | Meaning |
|---|---|
| `Pair<i64, string>` | A generic type with its type arguments |
| `any Shape` | A value of any type that conforms to the interface `Shape`, with dynamic dispatch |

Generic types, constraints, and interfaces are described in
[Generics and interfaces](../design/generics.md).

## Reading compound types

Prefix constructors (`?`, `[]`, `[N]`, `map[K]`, `task`, `weak`, `chan`) bind tighter than `!`. Angle
brackets group.

| Type | Means |
|---|---|
| `[]<E!T>` | An array of results |
| `E![]T` | One result whose success value is an array |
| `?<E!T>` | Three states: null, error, success |
| `E!?T` | Three different states: error, successful null, successful value |
| `fn(string) -> LoadError!Config` | A function that returns a result |
| `LoadError!<fn(string) -> Config>` | A result whose success value is a function |
| `?fn(i64) -> i64` | An optional function (the `?` applies to the whole `fn` type) |

A function type's return position takes a full type, including `!`. That is why
`fn(string) -> LoadError!Config` returns a result, and why you need `<...>` to
put a function inside a result.

### Rejected spellings

| Written | Why | Write instead |
|---|---|---|
| `[]E!T` | `!` is left over after `[]E` | `[]<E!T>` |
| `?E!T` | Same reason | `?<E!T>` |
| `E!E2!T` | The right side of `!` can't itself contain a bare `!` | `E!<E2!T>` |
| `??T` | `??` is one token, the coalescing operator | `?T`; see [Optionals collapse](#optionals-collapse) |
| `task E!T` | Same reason as `[]E!T` | `task <E!T>` |

### The error position

The left side of `!` must be a **named** type, such as `LoadError` or
`errors.LoadError`. To use a compound type as an error, name it with an alias:

```eit
type BatchErrors = []LoadError

fn load_all(paths: []string) -> BatchErrors![]Config { ... }
```

## Optionals collapse

An optional of an optional is the same type as a single optional:
`?<?T>` **is** `?T`. There is one `null`, and no second level of absence.

```eit
type MaybeName = ?string
let x: ?MaybeName = null      // x: ?string

let scores: []?i64 = [10, null]
let last = scores.pop()       // ?i64, not a nested optional
```

This happens wherever an optional meets another one: through aliases, through
generic substitution (`?T` with `T = ?i64`), and in APIs such as `pop()`,
`m.get(k)`, and `try?`.

The consequence is that `null` can mean either "no value here" or "a value
that is `null`". Code that needs to tell them apart asks directly: `m.has(k)`
for maps, `a.len()` for arrays, or a result type `E!?T`, which keeps the two
states separate.

## Type aliases

```eit
type Name = T
```

An alias is *transparent*: `Name` and `T` are the same type everywhere. Aliases
can only be declared at module level.

## Meaning, not storage

The spec promises observable states and behavior, never sizes or layouts.

- `?i64` covers every `i64` value plus `null`. The implementation is not
  required to reserve a sentinel, and it is not allowed to steal one.
- `[]?i64` may be stored as tagged elements or as values plus a presence
  bitmap.
- An enum's tag width and payload layout are unspecified.

The one exception is an [`@extern`](structs-classes-enums.md#extern-structs)
type, which has C layout so the host can read it directly. Its fields may also
use **storage types** (`i16`, `i32`, `u16`, `u32`, `isize`, `usize`, `ptr`,
`cstr`), which exist only there and read as ordinary scalars.

## Conversions

### Implicit conversions

Only three conversions happen implicitly:

- `T` → `?T`
- `T` → `E!T` (as a success)
- `fn(A) -> R` → `async fn(A) -> R` (a plain function can be used where an
  async one is expected; it costs nothing because both use the same call
  sequence)

```eit
fn find() -> ?Window {
    return window      // Window → ?Window
}

fn load() -> LoadError!Config {
    return config      // Config → LoadError!Config, same as .Ok(config)
}
```

### Integer literals

Integer literals take their type from context. The constant must fit:

```eit
let a: u8 = 300              // error: does not fit in u8
let b: i8 = -128             // OK
let c = 7                    // i64, the default
let d = 2.5                  // f64, the default
```

An integer literal may also initialize a float type when its value is
exactly representable there:

```eit
let e: f32 = 1               // OK: 1.0
let f: f32 = 16_777_217      // error: not exactly representable in f32
```

This applies to literals only. A variable of an integer type always needs an
explicit conversion.

### Explicit numeric conversions

Every other numeric conversion is explicit and written as a call:

```eit
let small = u8(big)                  // faults if big is out of range
let wrapped = u8(truncating: big)    // keeps the low 8 bits instead
let ratio = f64(count) / f64(total)
```

A failed checked conversion is a [fault](errors.md#faults).

Numeric conversions have these rules:

- Float to integer truncates toward zero, then faults if the truncated
  mathematical integer is outside the destination range. NaN and either
  infinity fault. Thus `i8(127.9)` is `127`, and `u8(-0.5)` is `0`.
- Integer to float and `f64` to `f32` never fault. They round to nearest,
  ties to even. Narrowing overflow produces the appropriately signed
  infinity; signed zero is preserved. `f32` to `f64` is exact.
- `truncating:` accepts integer sources and integer destinations only. It
  keeps the destination's low bits, interpreted with its signedness; a float
  source is a compile error.

Decimal float literals round to nearest, ties to even, directly to their
expected float type (or `f64` without an expected type). Overflow of that
literal's destination type is a compile error. Underflow to a subnormal or
zero is allowed, with the sign preserved. This differs from explicit float
narrowing, whose overflow produces infinity.

## Type inference

Inference is local and bidirectional:

- **Expected types flow down** into literals, lambdas, and enum shorthand.
  `let t: Theme = .Dark` works because `.Dark` is checked against `Theme`.
- **Inferred types flow up** from initializers. `let n = 3` gives `i64`.

There is no whole-program inference. See
[Bindings and functions](bindings-and-functions.md#return-types-and-inference) for the rules on
function return types.
