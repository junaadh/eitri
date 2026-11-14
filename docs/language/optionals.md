# Optionals

The current implementation supports optional strings, supported scalars, structs,
and enums, including collapsed repeated optional markers, `null`, implicit
conversion, equality, and `??` with matching value or optional fallbacks. These values can be stored
in structs and enum payloads and passed through script calls; optional host
signatures are not supported yet. Optional payload bindings and literal patterns
match only present values; `_` also matches null. Other optional types and
operations remain future work.

`?T` holds either a `T` or `null`. A `T` converts to `?T` implicitly.

```eit
var parent: ?Window = null
parent = main                    // Window → ?Window
```

## Reading through an optional

Using a `?T` as a `T` is a type error unless control flow has already
established that it is present:

```eit
let p: ?Window = find_window(windows, "Main")
app.log(p.title)                 // error: p may be null
```

There are four ways in:

### `?.` optional chaining

```eit
let title = p?.title             // ?string
```

`?.` reads a field or calls a method if the left side is present, and produces
`null` otherwise.

Chains **flatten**: `a?.b?.c` is `?C`, never `??C`. A null **short-circuits
the rest of the postfix chain**, including later `.`, calls, indexing, and the
evaluation of call arguments:

```eit
a?.b.c(f())       // if a is null, neither .b, .c, nor f() runs
```

Parentheses end the chain:

```eit
(a?.b).c          // error: (a?.b) is optional, so .c needs ?.
```

`?[` indexes through an optional, and joins the same chain:

```eit
let first = list?[0]           // ?T: null if list is null
let theme = config?.themes?["dark"]
```

Indexing still faults when the collection is present but the index is out of
bounds or the key is missing.

### `??` fallback

```eit
let title = p?.title ?? "Untitled"     // string
```

`a ?? b` yields `a`'s value if present, else evaluates `b`. `b` is evaluated
only when needed. `??` is right-associative, so `a ?? b ?? c` tries each in
turn.

`??` binds tighter than comparison but looser than arithmetic:

```eit
name ?? "x" == "y"     // (name ?? "x") == "y"
count ?? 0 + 1         // count ?? (0 + 1)
```

### `if let`

```eit
if let parent = window.parent {
    app.log(parent.title)          // parent: Window
}
```

### `guard let`

```eit
guard let main = find_window(windows, "Main") else {
    return
}
// main: Window from here to the end of the scope
```

The `else` block of a `guard` **must diverge**: `return`, `break`, `continue`,
or `panic(...)`. It can't fall through.

`while let` uses the same condition form and loops while the value is present.

## Narrowing

Comparing against `null` narrows a binding:

```eit
let p = find_window(windows, "Main")
if p != null {
    app.log(p.title)               // OK: p narrowed to Window
}
```

Narrowing applies **only to stable local bindings**:

- `let` bindings, and
- `var` locals that no closure captures.

Fields, map entries, and captured `var`s are never narrowed, because something
else could change them between the check and the use. Bind them first with
`if let`.

These conditions narrow a stable binding `x` of type `?T` to `T`:

| Condition | Narrowed in |
|---|---|
| `x != null` | The true branch |
| `x == null` | The false branch |
| `if x == null { … }` where the block diverges | The rest of the scope |
| `guard x != null else { … }` | The rest of the scope |
| `x != null && rest` | `rest`, and the true branch |
| `x == null \|\| rest` | `rest` |
| `while x != null { … }` | The loop body |

## Optionals in strings

An optional can't be interpolated directly:

```eit
app.log("score: ${score}")         // error: ?i64 is not formattable
app.log("score: ${score ?? 0}")    // OK
```

## Optionals in `match`

The patterns for `?T` are the patterns for `T` plus `null`. A binding in an
optional position only matches non-null values. See
[Pattern matching](pattern-matching.md#optionals).

## Optionals collapse

An optional of an optional is just an optional: `?<?T>` is `?T`. See
[Types](types.md#optionals-collapse).
