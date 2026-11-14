# Structs, classes, and enums

The current implementation accepts structs with up to 256 scalar, string or
nested aggregate fields, literals, field reads and writes, and copies in local
bindings and control flow. `let` fields cannot be changed after construction.
Struct update literals copy omitted fields from a final `..base` expression.
Fields occupy frame slots, so strings remain collector roots. Recursive inline
layouts are rejected. Structs can pass and return through script calls;
host-initiated calls reject aggregate signatures for now.

Methods using `extend` and explicit `self` work for structs and enums;
`mutating` struct methods require mutable receivers. Static extension functions
can be called through the type name or by bare name inside another method of the
type. Up to 256 methods per module are supported. Bare receiver field reads and writes work with local shadowing, including
nested writes and mutating calls on nested fields. Bare method calls resolve
on the receiver and retain its mutability rules. Literal, arithmetic and call defaults are supported;
defaults can construct enums and structs, including nested defaults. Expansion
is limited to 16 nested defaults and 65,536 total defaults per function;
captured names remain unsupported.
Enums support labeled or positional payload construction, and exhaustive case matching with positional
bindings execute on both interpreters. Structs and enums can contain each other,
with inline cycles rejected. Richer patterns remain future work. The rest of
this chapter is the language contract for those features.

## Declaration identity across contexts

A named declaration's stable key is **(module path, declaration name)**.
Declarations with the same key denote the same nominal type across contexts
when their shapes are structurally equal: the same declaration kind, the same
fields/cases/payloads in order, and equal field and payload types by this same
rule, recursively. Different keys do not denote the same nominal type merely
because their fields happen to have the same shape. Context-local type IDs never
determine cross-context identity. Transparent aliases retain their existing
meaning rather than creating a new nominal declaration.

Registration, artifact binding and runtime templates must reject a same-key,
different-shape pair with a typed error. This rule specifies type identity, not
object-instance identity or a global type registry.

## Structs

A `struct` is a value type. Assignment and argument passing copy it, and
`[]Vec2` stores its elements inline.

```eit
struct Vec2 {
    x: f32
    y: f32

    fn length() -> f32 {
        return sqrt(x * x + y * y)
    }

    mutating fn scale(k: f32) -> void {
        x *= k
        y *= k
    }
}
```

### Names inside methods

Inside a method, the receiver's fields and methods can be used without
`self.`, and the receiver itself is `self`. A bare name resolves in this order,
innermost first:

```
block locals → parameters → fields and methods of self → static functions of the type → module → imports
```

- **Locals and parameters shadow fields.** `self.x` always reaches the field:

  ```eit
  mutating fn set_x(x: f32) -> void {
      self.x = x
  }
  ```

- **Fields shadow module bindings** of the same name inside methods.
- In a `mutating` method, `self` can be assigned as a whole:
  `self = Vec2 { x: 0, y: 0 }`. In other methods it can't.
- `self` is only valid inside non-static methods.
- Closures inside a method capture `self` as a reference for classes and as a
  copy for structs. In a `mutating` method, a closure can't assign to `self`
  or its fields. See [Captures](bindings-and-functions.md#captures).

### Static functions

A `static fn` belongs to the type rather than to an instance. It has no
`self`, and it is called through the type name:

```eit
struct Vec2 {
    x: f32
    y: f32

    static fn new(x: f32, y: f32) -> Vec2 {
        return Vec2 { x: x, y: y }
    }

    static fn zero() -> Vec2 {
        return new(0, 0)          // static functions of the type are in scope
    }
}

let a = Vec2.new(3, 4)
let origin = Vec2.zero()
let make: fn(f32, f32) -> Vec2 = Vec2.new    // usable as a function value
```

- `static` is allowed in `struct` (including `@extern`) and `class` bodies. It
  can't be combined with `mutating`, but it can be `async`.
- A static function's body can't use `self` or bare field names, since there
  is no instance.
- `Vec2.new` resolves at compile time, so calling it is a direct call, the same
  as calling a module-level function.
- `new` is a convention, not a keyword. A type can have any number of static
  functions with any names.
- Importers call `pub` static functions of a `pub` type through the module:
  `geometry.Vec2.new(1, 2)`. See [Visibility](#visibility).

### Static constants

`static let` declares a constant that belongs to the type:

```eit
struct Vec2 {
    x: f32
    y: f32

    pub static let zero = Vec2 { x: 0, y: 0 }
    pub static let unit_x = Vec2 { x: 1, y: 0 }
}

let origin = Vec2.zero
```

Static constants are initialized like module bindings, in declaration order
when the module initializes, and the
[initialization-order check](modules.md#initialization-order) covers them.
There is no `static var`; mutable shared state lives in module bindings.

### Mutating methods

A method marked `mutating` may modify the struct's fields. It can only be
called through a `var`:

```eit
var a = Vec2 { x: 3, y: 4 }
a.scale(2)            // OK

let b = Vec2 { x: 3, y: 4 }
b.scale(2)            // error: b is let-bound
b.length()            // OK: length is not mutating
```

`mutating` is only allowed in `struct` bodies, including `@extern` structs,
which are ordinary values with C layout. Fields of an `@extern class` live in
host memory instead, and a `pub let` field there can't be assigned; see
[Permissions](#permissions).

### Read-only fields

A field declared with `let` can be set when the value is constructed, but
never assigned afterwards, even through a `var` or inside a `mutating`
method:

```eit
struct Account {
    let id: i64
    balance: i64 = 0
}

var a = Account { id: 7 }
a.balance += 10       // OK
a.id = 8              // error: id is a let field
```

In host classes, a `let` field is a read-only property; in `@extern` types, a
read-only field.

### Field defaults

```eit
struct Window {
    title: string
    width: i64 = 800
    height: i64 = 600
}

let w = Window { title: "Main" }
```

Like function defaults, a field default is evaluated at each construction site.

### Struct literals

```eit
let main = Window { title: "Main", width: 1200, height: 800 }
```

Every field without a default must be given.

### Struct update

`..base` copies every field of `base` that is not listed explicitly:

```eit
let settings = Window { title: "Settings", ..main }
```

`..base` must come last.

### Struct literals in headers

An unparenthesized struct literal is not allowed in the header of `if`, `for`,
`while`, `guard`, or `match`, because the `{` would be ambiguous with the
block:

```eit
if pos == Vec2 { x: 0, y: 0 } { ... }       // error
if pos == (Vec2 { x: 0, y: 0 }) { ... }     // OK
```

### Recursive types

A type can't contain itself **inline**, because its size would be infinite.
The compiler rejects any cycle of inline containment.

| Inline: continues the cycle | Through a reference: ends the cycle |
|---|---|
| Struct fields | `class` |
| `?T`, `E!T` | `[]T`, `map[K]V` |
| `[N]T` | `task T`, function types |
| Enum payloads | `@indirect` enum cases |

```eit
struct Node {
    value: i64
    next: ?Node              // error: Node contains itself inline
}                            //   use `class Node`, or a `[]Node` field

struct Tree {
    value: i64
    children: []Tree         // OK: arrays are references
}

class Window {
    title: string
    parent: ?Window = null   // OK: classes are references
}
```

Structs have no `@indirect`. Struct fields can be changed through `var`, so a
boxed field would need either a copy on every assignment or reference
counting to keep value semantics. That is the same trade-off that made
collections references. For self-referential records, use a class, or
collection fields for children.

## Classes

A `class` has shared identity. Assignment copies a garbage-collected
reference, and `[]Player` stores references.

```eit
class Player {
    position: Vec2
    health: i64

    static fn spawn_at(position: Vec2) -> Player {
        return Player { position: position, health: 100 }
    }
}

let p = Player { position: Vec2 { x: 0, y: 0 }, health: 100 }
let q = p
q.health = 50          // p.health is now 50 too
```

- **Class literals use struct literal syntax.** Each class literal allocates
  a new garbage-collected object. That is its known cost. Field defaults work
  as for structs.
- **`..base` makes a new object.** `Player { health: 50, ..p }` copies `p`'s
  fields into a new object. The copy is shallow: class references among those
  fields are shared, not cloned.
- **Construction with rules** goes through a static function, such as
  `Player.spawn_at(...)`. Private fields make it the only way in; see
  [Visibility](#visibility).
- Methods on classes are never `mutating`. Every method can modify the object.
- `let p` forbids rebinding `p`, not mutating the object.
- Classes have no inheritance in this draft. All methods resolve statically.

- **`==` compares identity.** Two class instances are equal only if they are
  the same object. For content comparison, define a method such as
  `fn equals(other: Player) -> bool`. See
  [Equality and ordering](expressions.md#equality-and-ordering).



## Enums

Enums are tagged unions. A case may carry a payload of named fields.

```eit
enum Theme {
    Light
    Dark
    Custom(color: string)
}
```

All case names are PascalCase, including the compiler-known `.Ok` and `.Err`
of `E!T`.

### Recursive enums: `@indirect`

An enum can refer to itself through a case marked `@indirect`:

```eit
enum Expr {
    Number(value: f64)
    @indirect Add(left: Expr, right: Expr)
    @indirect Neg(operand: Expr)
}

@indirect enum List {          // every case is indirect
    Empty
    Cons(head: i64, tail: List)
}
```

- An `@indirect` case stores its payload in a heap box. That is **one
  allocation each time the case is constructed**, and the attribute shows
  exactly where that cost is. It is never inferred.
- Value semantics are kept without copying. Enum payloads can't be changed in
  place, so sharing a box is never observable.
- `@indirect` on the enum applies to every case with a payload.
- Cases that reach the enum only through a reference, such as `[]Json` or
  `map[string]Json`, need no attribute.

### Constructing cases

```eit
let a = Theme.Dark
let b = Theme.Custom(color: "#ff8800")
let c: Theme = .Light                        // shorthand, expected type known
let d: Theme = .Custom(color: "#000000")
```

The `.Case` shorthand requires an expected type. It works anywhere one is
available: annotated bindings, arguments, return values, match arms, and
nested inside other shorthand such as `.Ok(.Light)`.

### Using cases

Enum values are taken apart with [`match`](pattern-matching.md):

```eit
match theme {
    .Light => "light",
    .Dark => "dark",
    .Custom(color) => color,
}
```

### Enum methods

After its cases, an enum can declare methods, static functions, and static
constants. Inside a method, `self` is the enum value:

```eit
enum Theme {
    Light
    Dark
    Custom(color: string)

    pub fn background() -> string {
        return match self {
            .Light => "#ffffff",
            .Dark => "#111111",
            .Custom(color) => color,
        }
    }

    pub static fn from_name(name: string) -> Theme {
        return match name {
            "light" => .Light,
            "dark" => .Dark,
            _ => .Custom(color: name),
        }
    }
}
```

Enums have no fields and no `mutating` methods: an enum value changes only by
assigning a new one.

## Interfaces

A struct, class, or enum can declare that it conforms to interfaces:

```eit
interface Shape {
    fn area() -> f32
}

struct Circle: Shape {
    radius: f32

    fn area() -> f32 {
        return 3.14159 * radius * radius
    }
}
```

Types can also take type parameters: `struct Pair<A, B> { ... }`. Both are
described in [Generics and interfaces](../design/generics.md).

## Extensions

`extend` adds methods or interface conformances to a type after its
declaration:

```eit
extend Vec2: Shape {
    fn area() -> f32 {
        return 0
    }
}
```

- An extension must be in the module that declares the type **or** the
  module that declares the interface it adds. Two modules can then never give
  conflicting conformances: for both to declare `extend Vec2: Shape`, the
  type's module would have to import the interface's module and the reverse,
  which is an import cycle and already a compile error. A module that
  declares the same conformance twice is rejected directly.
- A conformance is visible wherever both the type and the interface are.
- Extensions can't add stored fields, which would change the type's layout.
- To add helpers to a type from somewhere else, write ordinary functions.

## Visibility

Fields, methods, and static functions are private to their module unless
marked `pub`. This is the module rule for top-level items, applied to members.

```eit
// geometry.eit
pub struct Rect {
    pub width: f32
    pub height: f32
    area_cache: f32 = 0              // private

    pub static fn new(w: f32, h: f32) -> Rect {
        return Rect { width: w, height: h }
    }

    pub fn area() -> f32 {
        return width * height
    }

    fn recompute() -> void { ... }   // private
}
```

- **The module is the boundary, not the type.** Code in the defining module
  sees every member. A single-file script never needs `pub` on members.
- **Importers can read and assign `pub` fields.** For a field that should be
  read-only outside, expose a `pub fn` getter. Methods resolve statically, so a
  trivial getter can be inlined and costs nothing.
- **Literals from outside the module** are allowed only if every field
  without a default is `pub`. Private fields with defaults take their
  defaults. To make `Rect.new(...)` the only way to construct a `Rect`, give it
  a private field without a default.
- **Destructuring from outside** may name only `pub` fields. Copying a whole
  struct, by assignment or `..base`, carries its private fields along, which
  is safe because importers can't read them.
- **Enum cases and payloads are as visible as the enum.** Hiding individual
  cases would break exhaustive `match` in importers.
- **The check is compile-time only.** Visibility costs nothing at run time.

Lua has no privacy, so marking members `pub` reaches every Lua outcome.
Visibility is something Eitri adds on top.

## Extern structs

`@extern` gives a type **C layout**, so the host can read it directly. It
comes in two forms with different meanings:

| Form | What it is | Semantics | Field access |
|---|---|---|---|
| `@extern struct` | A **value with C layout** | Copied on assignment, like any struct. Stored inline in arrays, so a `[]Vertex` is one contiguous C array | One load or store at a fixed offset |
| `@extern class` | A **view of an object in host memory** | A reference: a [host object](../implementation/embedding.md#host-objects) whose fields are memory rather than properties | A generation check, then one load or store |

```eit
@extern
pub struct Vertex {
    pub x: f32
    pub y: f32
    pub color: u32              // a storage type; reads as i64
}

let quad: []Vertex = [
    Vertex { x: 0, y: 0, color: 0xffffffff },
    Vertex { x: 1, y: 0, color: 0xffffffff },
    Vertex { x: 1, y: 1, color: 0xffffffff },
    Vertex { x: 0, y: 1, color: 0xffffffff },
]
gpu.upload(quad)                // the host borrows the array: no copy
```

- `@extern struct` may be declared in any module; it is only a layout.
- `@extern class` may be declared only in modules the host registers.
- A script can never cause memory unsafety through either form: there are no
  raw addresses and no pointer dereferences in script code.

### Layout

- Eitri computes the layout from the declaration with **C rules**: fields in
  order, natural alignment, and padding.
- For types shared with the host, registration **checks** the computed layout
  against the host's real type: size, alignment, and every offset. The Rust
  `#[derive(eitri::Extern)]` emits the host's layout, and `eitri-bindgen` emits
  `static_assert`s for C. A mismatch is a registration error.
- Byte order and alignment are the target's own, and are part of that check.

### Field types

| Field type | Notes |
|---|---|
| `i8`, `i64`, `u8`, `u64`, `f32`, `f64` | As usual |
| `bool` | One byte. Any non-zero byte reads as `true`; writes store 0 or 1 |
| Payload-less enum | Stored as an `i32`, as C enums are. Reading a value that isn't a case **faults** |
| `[N]T` | Stored inline, for any allowed `T` |
| Another `@extern struct` | Stored inline |
| Storage types (below) | C widths that Eitri has no ordinary type for |

`string`, `?T`, classes, collections, `dyn`, and functions can't be fields of
an `@extern` type, because none of them has a C layout.

### Storage types

These types exist **only as field types of `@extern` types**. Reading
widens them to an ordinary scalar, so script arithmetic only ever sees the
core types:

| Storage type | Read as | Write |
|---|---|---|
| `i16`, `i32`, `isize` | `i64`, sign-extended | Range-checked; a value out of range **faults** |
| `u16`, `u32` | `i64` | Range-checked |
| `usize` | `u64` | Range-checked on 32-bit targets |
| `ptr` | Can't be read or written | Holds layout only |
| `cstr` | `?string`: `null` for a null pointer, otherwise a copy of the NUL-terminated bytes. Invalid UTF-8 **faults** | Can't be written |

### Pointers

A raw pointer has no lifetime the VM can check, so scripts never follow one.

- A pointer field is declared with type `ptr`, usually private. It keeps the
  layout right, and scripts can't read or write it.
- To follow a pointer, the host exposes a property or method that returns an
  `@extern class` handle, which is generation-checked:

```eit
@extern
pub class Node {
    pub value: i32
    next: ptr                         // layout only
    pub fn next_node() -> ?Node       // the host follows the pointer
}
```

### Strings

- A `cstr` field is a pointer to a NUL-terminated C string. Scripts can read
  it but never assign it, because no one would own the new memory.
- A fixed buffer such as `name: [32]u8` stores text inline. Read it with
  `string.from_c_buffer(buf)`, which stops at the first NUL and returns
  `Utf8Error!string`. Scripts can write fixed buffers, since the bytes are part
  of the value.

### Permissions

Visibility doubles as permission on `@extern` fields:

| Declared as | Scripts may |
|---|---|
| `pub x: T` | Read and write |
| `pub let x: T` | Read only |
| `x: T` (private) | Neither; the field holds layout only |

### Owned resources

A file handle, texture, or socket inside a C struct has an owner and a
destructor, and Eitri doesn't model ownership in layouts. Such a resource is a
host object, a `class` in the host module, with methods such as `close()`.
Scripts release it with `defer` or
[`runtime.on_collect`](../design/runtime-features.md#cleanup-on-collection).
If its handle must sit inside a layout, the field is a private `ptr` or
integer.

### Costs

| Access | Cost |
|---|---|
| `@extern struct` field | A load or store at a fixed offset |
| Storage type read | A load plus sign or zero extension |
| Storage type write | A range check plus a store |
| `@extern class` field | A generation check plus a load or store |
| Passing a `[]T` of an `@extern struct` to the host | A pointer and a length; no copy |
| Reading a `cstr` | A copy plus a UTF-8 check |

For hot loops over host data, a host can copy the data into an array of
`@extern struct` values, which needs no generation checks.

Packed layouts, explicit offsets (reserved as `@extern(packed)`), and other
storage widths for enum fields are deferred.
