# Design note: the dynamic type

**Status:** decided.

`dyn` holds a value of any type, together with that type's ID. It is for data
whose shape is only known at run time: JSON, plugin data, and loosely
structured configuration. It gives Eitri Lua's flexibility exactly where a
script asks for it, and nowhere else.

```eit
import json from "eitri:json"

let config: dyn = try json.parse(text) else e => Error { message: "${e}" }

if let size: i64 = config["window"]["size"] {
    resize(size)
}

match config["theme"] {
    name: string => use_theme(name),
    null => use_theme("dark"),
    _ => app.log("bad theme: ${config["theme"]}"),
}
```

## Into `dyn`

Any value converts to `dyn` implicitly. The `dyn` in the target type is what
makes the cost visible.

```eit
let a: dyn = 42
let b: dyn = "text"
let c: dyn = Vec2 { x: 1, y: 2 }
let d: dyn = null
```

Collection literals with an expected type of `dyn` take a dynamic shape:

```eit
let settings: dyn = ["name": "main", "size": 3, "tags": ["a", "b"]]
// a map[string]dyn whose "tags" entry holds a []dyn
```

## Out of `dyn`

Values leave `dyn` only through a type test, using forms Eitri already has:

```eit
if let n: i64 = value { ... }            // typed if let
guard let s: string = value else { return }

match value {                            // type patterns
    n: i64 => ...,
    s: string => ...,
    null => ...,
    _ => ...,
}
```

- A type test matches only the exact type. An `i64` inside a `dyn` matches
  `i64`, and does not match a different scalar type such as `i8` or `u64`.
  Converting `42` of type `i64` to `dyn` therefore matches an `i64` pattern.
- A `dyn` can hold any type, so a `match` on `dyn` always needs a catch-all
  arm.
- There is no unchecked cast. A failed test takes the `else` branch or the
  next arm, never a fault.

## Dynamic access

Operations on a `dyn` produce another `dyn`, with Lua's behavior for missing
data:

| Operation | Result |
|---|---|
| `d.name` | The field `name`, if the held value has one; otherwise `null` |
| `d[k]` | The element or entry, for arrays and maps; `null` if missing or out of range |
| `d(args...)` | Calls the held function with dynamic arguments. Faults if it isn't a function, or if an argument has the wrong type |
| `d.name = v`, `d[k] = v` | Assigns, if the target exists and the type fits; otherwise faults |
| Any of these on a `null` | `null` for reads; a fault for calls and assignments |

- Field access respects [visibility](../language/structs-classes-enums.md#visibility)
  as seen from where the access is written. Dynamic access can't reach
  private fields.
- **No arithmetic or comparison other than `==` on `dyn`.** Take the value out
  first. Hidden type checks and branches never appear in ordinary-looking
  code.

## Other operations

| Operation | Behavior |
|---|---|
| `a == b` | Equal if both hold the same type and that type's `==` says equal |
| `d.type_name()` | The name of the held type, such as `"i64"` or `"Vec2"` |
| `d.is_null()` | Whether it holds `null` |
| `"${d}"` | Formats the held value; `null` formats as `null` |
| As a map key | Allowed, with the held value's hash. `NaN` keys fault |

## JSON

`eitri:json` is a standard module, available if the host allows it:

| Function | Returns |
|---|---|
| `json.parse(text)` | `JsonError!dyn`, built from `map[string]dyn`, `[]dyn`, `string`, `f64`, `i64`, `bool`, and `null` |
| `json.stringify(d)` | `JsonError!string`. Fails for values JSON can't represent |

JSON numbers without a fraction or exponent parse as `i64` when they fit, and
as `f64` otherwise.

## Representation and cost

- **A `dyn` is a type ID plus one machine word.** Scalars that fit are stored
  in the word; anything else is a pointer. Storing a struct boxes it, which is
  one allocation.
- **Field access** looks up the field by name in the held type's metadata.
  Each access site has a **guarded inline cache**: it remembers the type ID
  it last saw and that field's offset. When the guard matches, the access is
  a compare and a load. This is "specialize what is learned", applied where
  the dynamic cost is.
- **Type tests** compare type IDs: one integer comparison.
- **Code that doesn't use `dyn` pays nothing.**

## Why this shape

- **Implicit in, tested out.** Wrapping a value in `dyn` can't go wrong, so
  it needs no ceremony. Unwrapping can, so it always goes through a test that
  the compiler forces you to handle.
- **`null` for missing data, faults for misuse.** Reading a missing key is
  normal when exploring loosely structured data, as in Lua. Calling a
  non-function is a bug.
- **No dynamic arithmetic.** Lua coerces strings and numbers in arithmetic.
  Eitri makes the conversion explicit, so `+` on two values always means one
  known instruction sequence.
