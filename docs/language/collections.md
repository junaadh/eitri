# Collections

The current implementation supports fixed arrays of supported scalar, string, class, struct, enum, result, and optional
elements, with lengths 0 through 256
in literals, copies, aggregate fields, equality, script calls, and indexed reads.
Indexed writes to mutable locals, mutable struct fields, and class fields also
work. Nested array types and dynamic collections remain future
work. An unannotated array literal denotes a dynamic array and is rejected until
that feature lands.

## Literals

```eit
let names = ["Ada", "Lin"]                       // []string
let empty: []string = []
let corner: [4]f32 = [0, 0, 1, 1]
let shortcuts = ["save": "Ctrl+S"]               // map[string]string
let none: map[string]i64 = [:]
```

A collection literal gets its type from an annotation, or else from its
elements. Empty literals (`[]` and `[:]`) always need an expected type.

The parser reads the first element, and a following `:` makes it a map
literal.

## Sharing: collections are references

`[]T` and `map[K]V` behave like classes. Assigning or passing one copies a
reference, and both names see the same collection:

```eit
let a = [1, 2, 3]
let b = a
b.push(4)              // a is now [1, 2, 3, 4] too
let c = a.copy()       // an independent copy
```

`[N]T` is different: it is a value. It is stored inline and copied on
assignment, like a struct.

| | `[]T`, `map[K]V` | `[N]T` |
|---|---|---|
| Assignment | Copies a reference | Copies the elements |
| `let` forbids | Rebinding only; contents can still change | Rebinding and changing elements |
| Allocation | One per literal or `copy()` | None; stored inline |

Consequences:

- **`let` works as for classes.** `let xs = [1]` can't be rebound, but
  `xs.push(2)` is allowed.
- **Structs share their collections.** Copying a struct copies the
  references in it, so two copies of a struct share any array or map fields.
  A `let`-bound struct forbids `s.items = other` but allows
  `s.items.push(x)`. This is Go's model.
- **Copies are always visible.** The only way to duplicate a collection is
  `copy()`, which makes a shallow copy. Elements that are classes or
  collections are shared, not cloned.
- **Strings are unaffected.** They are immutable, so sharing is not
  observable.

### Why references

Value semantics without copying on every assignment, as Swift does it, copies
a shared buffer on its first write. That needs to know whether the buffer is
shared, which Swift learns from reference counting. Eitri uses a tracing
garbage collector, so the choices would be reference counting for collections,
which costs something on every assignment and drop, or a "shared" flag that
never clears, which causes copies nobody can predict. References make every
copy explicit, keep assignment to a single pointer copy, and match Lua tables.

## Growable arrays: `[]T`

`[]T` is a growable array. Elements of a `struct` type are stored inline.
Elements of a `class` type are stored as references.

```eit
let list: []i64 = [1, 2, 3]
list[0] = 10
list.push(4)
let first = list[0]
```

Indexing `a[i]` **faults** if `i` is out of bounds. Indices are 0-based.

### Array methods

These are built into the compiler, since users can't yet write generic
functions.

| Method | Returns | Behavior |
|---|---|---|
| `len()` | `i64` | Number of elements |
| `push(x)` | `void` | Appends |
| `pop()` | `?T` | Removes and returns the last element; `null` if the array is empty |
| `insert(i, x)` | `void` | Inserts before index `i`; faults unless `0 <= i <= len()` |
| `remove_at(i)` | `T` | Removes and returns the element at `i`; faults if out of bounds |
| `clear()` | `void` | Removes every element |
| `copy()` | `[]T` | A shallow copy |
| `contains(x)` | `bool` | Whether an element `==` `x` |
| `index_of(x)` | `?i64` | The first index whose element `==` `x`, or `null` |
| `equals(other)` | `bool` | Same length and every element `==`. Compare with `==`, which is identity |
| `slice(r)` | `[]T` | A new array with the elements in the range `r`; faults if `r` is out of bounds |

`pop()` returns `?T` because an empty array is a normal outcome. For a
`[]?T`, the optionals [collapse](types.md#optionals-collapse) to `?T`; check
`len()` first if an empty array and a `null` element must be told apart.
`remove_at` faults like indexing; see
[Choosing `E!T`, `?T`, or a plain value](errors.md#choosing-et-t-or-a-plain-value).

Sorting, filtering, mapping, and other helpers belong to the standard
library, written as generic functions.

## Fixed arrays: `[N]T`

`[N]T` holds exactly `N` values inline. The literal must have exactly `N`
elements.

```eit
let corner: [4]f32 = [0, 0, 1, 1]
let [x0, y0, x1, y1] = corner           // destructuring: fixed arrays only
```

## Maps: `map[K]V`

Maps follow one rule for every value type:

| Operation | Type | Behavior |
|---|---|---|
| `m[k]` | `V` | **Faults** if `k` is missing |
| `m.get(k)` | `?V` | `null` if missing |
| `m.has(k)` | `bool` | Whether `k` is present |
| `m[k] = v` | statement | Inserts or updates |
| `m.remove(k)` | | Deletes the entry |

```eit
var settings: map[string]string = ["theme": "dark"]

let theme = settings["theme"]                    // string, or fault
let font = settings.get("font") ?? "mono"        // string
if settings.has("theme") { ... }
settings["font"] = "serif"
settings.remove("theme")
```

A missed `get` returns `null`, so it never allocates.

### Map methods

| Method | Returns | Behavior |
|---|---|---|
| `len()` | `i64` | Number of entries |
| `get(k)` | `?V` | See above |
| `has(k)` | `bool` | See above |
| `remove(k)` | `void` | Deletes the entry, if present |
| `clear()` | `void` | Removes every entry |
| `copy()` | `map[K]V` | A shallow copy, keeping insertion order |
| `equals(other)` | `bool` | Same keys, with `==` values. Insertion order doesn't matter |

### Keys

Any type that supports [`==`](expressions.md#equality-and-ordering) can be a
map key, and keys hash the same way they compare:

- Values (numbers, strings, structs, enums, `[N]T`) are matched by content.
- References (classes, arrays, maps, tasks, functions) are matched by
  identity, so an object can be a key, as a table can be in Lua.
- `?T` keys are allowed, so `null` can be a key.
- Float keys follow IEEE equality: `-0.0` and `0.0` are the same key.
  Inserting `NaN` as a key **faults**, because it could never be found again.

The hash function is unspecified. Iteration follows insertion order, so
hashing never affects what a script observes.

### Maps with optional values

Storing `null` in a `map[K]?V` keeps the entry **present**. `get` returns
`?V`, with the optionals [collapsed](types.md#optionals-collapse), so a
missing key and a stored `null` both give `null`. Use `has` to tell them
apart:

```eit
var scores: map[string]?i64 = ["ada": 10, "lin": null]

if !scores.has(name) {
    print("no entry")
} else if let score = scores.get(name) {
    print("score: ${score}")
} else {
    print("present, no score")
}
```

### Order

Maps keep **insertion order**. Iteration visits entries in the order their
keys were first inserted.

- Updating an existing key keeps its position.
- Removing a key and inserting it again moves it to the end.

Order is observable, so it is part of a map's meaning: the same script
visits entries in the same order on every run and every host. The expected
representation is a dense entry array plus a hash index, so iteration walks
contiguous memory, and removal leaves a tombstone that is compacted when the
map grows.

## Iteration

`for` works on arrays, fixed arrays, maps, and ranges:

```eit
for window in windows { ... }             // elements
for i, window in windows { ... }          // index and element
for key in shortcuts { ... }              // keys
for key, value in shortcuts { ... }       // key and value
for _, value in shortcuts { ... }         // values only
for i in 0..n { ... }                     // ranges: one name only
```

| Iterating | One name | Two names |
|---|---|---|
| `[]T`, `[N]T` | element `T` | index `i64`, element `T` |
| `map[K]V` | key `K` | key `K`, value `V` |
| range | the number | not allowed |

- `_` can replace either name.
- Loop bindings are `let`. Struct elements and values are copies, so to
  update one, assign through the collection (`m[key] = ...`, `a[i] = ...`).
- One name on a map gives keys, so no `[]K` has to be built just to loop over
  them.
- For a `map[K]?V`, the value has type `?V`. Entries holding `null` are
  present, so they are visited.

### Changing a collection while iterating it

| During iteration of a collection | Allowed? |
|---|---|
| Assigning to an existing map key, `m[key] = v` | Yes. The loop sees the new value if it hasn't reached that entry yet |
| Assigning to an array element, `a[i] = v` | Yes, with the same visibility |
| Inserting a new map key, or `m.remove(k)` | **Fault** |
| Changing an array's length | **Fault** |

The check is one comparison per iteration step, against the length or a
modification counter. It can't be done at compile time in general, because a
class field can hold a collection that other code also reaches. As with
out-of-bounds indexing, a fault makes the bug loud instead of quietly skipping
or repeating entries.

To remove entries while scanning, collect the keys first:

```eit
var stale: []string = []
for key, entry in cache {
    if entry.expired { stale.push(key) }
}
for key in stale {
    cache.remove(key)
}
```

Collections are shared references, so this check can't be replaced by
iterating a snapshot without a hidden copy.
