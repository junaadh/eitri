# Pattern matching

The current implementation supports exhaustive matches on enums using `.Case`,
positional payload bindings or `_`, and a catch-all `_` arm. Parenthesized
case patterns must supply exactly one pattern per payload field. The remaining
patterns and guards below are the language contract for later work.

## `match`

```eit
let label = match theme {
    .Light => "light",
    .Dark => "dark",
    .Custom(color) if color != "" => color,
    .Custom(_) => "custom",
}
```

`match` is an expression. Every arm's body must have a compatible type when the
value is used.

- Arms are separated by commas. The comma is optional after an arm whose body
  is a block.
- A trailing comma after the last arm is allowed.
- The subject is a statement header, so an unparenthesized struct literal is
  not allowed there.

```eit
match event {
    .Click(x, y) => {
        handle_click(x, y)
    }                                   // no comma needed after a block
    .Key(code) => handle_key(code),
    _ => {},
}
```

## Exhaustiveness

`match` is **exhaustive**: the arms together must cover every possible value of
the subject's type. A non-exhaustive match is a compile error.

- An arm with a guard (`if ...`) **never** counts toward exhaustiveness, since
  the guard might be false.
- `_` and a bare binding cover everything (with the optional caveat below).
- For types with unbounded values (integers, strings), a catch-all arm is
  required.

## Patterns

| Pattern | Matches |
|---|---|
| `_` | Anything, including `null`. Binds nothing |
| `name` | Any value (non-null in an optional position), binding it to `name` |
| `name: T`, `_: T` | A [`dyn`](../design/dynamic.md) value that holds a `T`, binding it as a `T` |
| `42`, `-1`, `3.5`, `"dark"`, `'a'`, `true` | That literal |
| `null` | `null` |
| `.Case` | An enum case with no payload, or any payload |
| `.Case(p1, p2)` | An enum case whose payload fields match `p1`, `p2` in order |
| `.Case(field: p)` | Same, naming the payload field |
| `Type.Case(...)` | A fully qualified case |
| `p1 \| p2` | Either alternative |
| `(p)` | Grouping |

### Identifiers vs. case paths

A lone identifier is always a **binding**. A dotted name (`.Dark` or
`Theme.Dark`) is always an **enum case**. So `Dark` alone would bind a new
variable called `Dark`, not match the case.

### Type patterns

On a `dyn` subject, `name: T` matches when the value holds a `T`, and binds
it with type `T`:

```eit
match value {            // value: dyn
    n: i64 => app.log("number ${n}"),
    s: string => app.log("text ${s}"),
    null => app.log("nothing"),
    _ => app.log("something else"),
}
```

A `dyn` can hold any type, so a match on `dyn` always needs a catch-all arm.

In a payload, `name:` is always a field label. A positional type pattern goes
in parentheses: `.Wrap((v: i64))`.

### Payload patterns

```eit
match result {
    .Err(.NotFound(path)) => app.log("missing ${path}"),
    .Err(.Invalid(message: m)) => app.log("invalid: ${m}"),
    .Ok(config) => apply(config),
}
```

Payload patterns nest. They can be positional or labeled with the payload's
field name.

`.Case` without parentheses matches the case whatever its payload, so
`.Rect` stands for `.Rect(_, _, _, _)`. Positional and labeled payload
patterns can be mixed, with positional ones first, as with call arguments.

### Range patterns

```eit
match c {
    'a'..='z' | 'A'..='Z' => letters += 1,
    '0'..='9' => digits += 1,
    _ => {},
}
```

`a..=b` and `a..b` match integers and `char`s in the range. Exhaustiveness
checking understands them.

### Alternatives

```eit
match theme {
    .Light | .Dark => "builtin",
    .Custom(c) => c,
}
```

All alternatives must bind the **same names with the same types**.

### Guards

```eit
match score {
    s if s >= 90 => "A",
    s if s >= 80 => "B",
    _ => "C",
}
```

A guard is any boolean expression of `||` level or tighter, so `=>` is never
mistaken for a lambda.

## Optionals

The patterns for `?T` are the patterns for `T`, plus `null`.

- A **binding** in an optional position matches only non-null values, and binds
  the inner type `T`.
- `_` matches anything, including `null`.

```eit
match find_window(windows, "Main") {      // ?Window
    null => app.log("none"),
    w => app.log(w.title),               // w: Window
}
```

This rule composes with results:

```eit
match load_score(name) {          // LoadError!?i64
    .Ok(null) => print("present, no score"),
    .Ok(score) => print("score: ${score}"),
    .Err(_) => print("no entry"),
}
```

Here `.Ok(score)` doesn't match `.Ok(null)`, because `score` is a binding in an
optional position. The three arms together are exhaustive.

## Not yet supported

- Struct patterns in `match`. Structs can be destructured only in `let`/`var`.
- Array patterns.
