# Expressions and control flow

## Operator precedence

From loosest to tightest:

| Level | Operators | Associativity |
|---|---|---|
| 1 | `=>` (lambda), `try … else` (error mapping) | — |
| 2 | `\|\|` | left |
| 3 | `&&` | left |
| 4 | `==` `!=` `<` `<=` `>` `>=` | **none**: `a < b < c` is a syntax error |
| 5 | `??` | right |
| 6 | `..` `..=` | none |
| 7 | `\|` | left |
| 8 | `^` | left |
| 9 | `&` | left |
| 10 | `<<` `>>` | left |
| 11 | `+` `-` `&+` `&-` | left |
| 12 | `*` `/` `%` `&*` | left |
| 13 | prefix `-` `!` `~`, `try`, `try?`, `await`, `spawn` | — |
| 14 | postfix `.` `?.` `()` `[]` | left |

Notes:

- `??` binds **tighter** than comparison: `name ?? "x" == "y"` means
  `(name ?? "x") == "y"`.
- Bitwise operators bind tighter than comparison, unlike C:
  `flags & MASK == 0` means `(flags & MASK) == 0`.
- `try`, `try?`, and `await` take a single postfix chain:
  `try? load(p) ?? fallback` means `(try? load(p)) ?? fallback`. When combined,
  `try` comes first: `try await fetch(url)`.
- Assignment is a statement, not an operator. See
  [Bindings and functions](bindings-and-functions.md#assignment).

## Arithmetic

`+`, `-`, and `*` **fault on overflow**. Use the wrapping forms when you want
modular arithmetic:

```eit
let hash = hash &* 31 &+ byte
```

Both operands of a binary arithmetic operator must have the same type. There is
no implicit widening:

```eit
let a: u8 = 5
let b: i64 = 7
let c = a + b            // error: u8 and i64
let d = i64(a) + b       // OK
```

### Division and remainder

| Operation | Integers | Floats |
|---|---|---|
| `a / b` | Truncates toward zero: `-7 / 2 == -3` | IEEE division |
| `a % b` | Remainder with the sign of `a`: `-7 % 2 == -1` | IEEE remainder |
| Division by zero | **Fault** | `inf`, `-inf`, or `NaN`; no fault |
| `MIN / -1` (signed) | **Fault**: the result overflows | — |
| `MIN % -1` (signed) | `0`: the remainder fits, so no fault | — |

Floor division and floor modulo, as Lua's `//` and `%` do, are library
functions: `floor_div(a, b)` and `floor_mod(a, b)`.

### Shifts

- The shift amount must be in `0..bits` for the operand's type. Anything else
  **faults**.
- Bits shifted out are dropped; shifting is not checked for overflow.
- `>>` on a signed integer keeps the sign; on an unsigned integer it fills
  with zeros.

## Equality and ordering

**Values compare by content. References compare by identity.**

| Type | `==` compares |
|---|---|
| Integers, `bool` | Value |
| Floats | IEEE rules: `NaN != NaN`, and `-0.0 == 0.0` |
| `string`, `bytes` | Content |
| `struct`, `[N]T` | Every field or element. Available automatically when all of them support `==` |
| `enum` | Same case and equal payloads, under the same condition |
| `?T`, `E!T` | `null == null`; otherwise the inner values |
| `class`, `[]T`, `map[K]V`, `task T` | **Identity**: the same object |
| Function values | Identity: the same function, or the same closure object |

`!=` is the negation of `==`.

```eit
let a = [1, 2]
let b = [1, 2]
a == b            // false: different arrays
a.equals(b)       // true: same elements
a == a            // true
```

- **Both operands have the same type**, except for the implicit `T → ?T`
  conversion, so `count == null` and `maybe == 5` both work.
- **`==` never runs user code.** There is no operator overloading. Comparing
  references by content is a method call: `equals()` on arrays and maps, or a
  class's own `fn equals(other: T) -> bool`. The call makes the cost visible.
- **Function identity** lets a script find and remove a handler it registered
  earlier. Two uses of the same top-level function are equal; each closure that
  captures something is a distinct object.
- **Ordering** (`<`, `<=`, `>`, `>=`) is defined on numbers, `char` (by code
  point), and `string` (byte by byte, which matches code point order). No other
  type is ordered; sorting other types takes a comparison function.
- **`+` on strings and `bytes`** concatenates. A chain of `+` compiles to one
  allocation. See [Strings](strings.md#concatenation).

How the VM keeps equality cheap (generated comparisons per type, cached
string hashes) is described in [Identity and hashing](../implementation/identity.md).

## Booleans

Conditions must be `bool`. Nothing is implicitly true or false: `0`, `""`,
empty arrays, and `null` are not `false`.

```eit
if count { ... }          // error: i64 is not bool
if count != 0 { ... }     // OK
```

`&&` and `||` short-circuit. `!` is logical not and `~` is bitwise not.

## Blocks

A block is a sequence of statements in `{ }`. When a block is used as an
expression, its value is its **final expression statement**.

```eit
let area = {
    let w = right - left
    let h = bottom - top
    w * h
}
```

## `if`

```eit
if ready {
    start()
} else if waiting {
    wait()
} else {
    stop()
}
```

`if` is an expression. Using its value requires an `else`, and the branch
types must be compatible:

```eit
let bg = if app.dark_mode { "#111111" } else { "#ffffff" }
```

The condition can be a `bool` expression or `let name = optional`
(see [Optionals](optionals.md#if-let)). `else` must be on the same line as the
preceding `}`.

## `guard`

```eit
guard count > 0 else {
    return
}
guard let main = find_window(windows, "Main") else {
    return
}
```

The `else` block **must diverge**: it has to end in `return`, `break`,
`continue`, or `panic(...)`.

## Loops

```eit
for window in windows { ... }      // each element
for i, window in windows { ... }   // index and element
for i in 0..n { ... }              // 0, 1, …, n - 1
for i in 0..=n { ... }             // 0, 1, …, n
for key, value in some_map { ... } // insertion order
while running { ... }
while let job = queue.next() { ... }
```

`break` exits the innermost loop and `continue` skips to its next iteration.

What each form binds, iteration order, and the rules for changing a collection
while iterating it are in [Collections](collections.md#iteration).

### Ranges

`a..b` (end excluded) and `a..=b` (end included) are values of type
`Range<T>`, for an integer type `T`. A range is just two integers, so it
costs nothing to create.

```eit
let r = 0..10                  // Range<i64>
for i in r { ... }
r.contains(5)                  // true
r.len()                        // 10
let middle = items.slice(2..5)
let head = name.slice(0..3)    // byte offsets, as for slice(start, end)
```

### Labeled loops

A loop can have a label, so `break` and `continue` can target an outer loop:

```eit
outer: for row in grid {
    for cell in row {
        if cell.blocked { continue outer }
        if cell.goal { break outer }
    }
}
```

This covers what Lua scripts use `goto` for. Loops don't produce values.

## `match`

See [Pattern matching](pattern-matching.md).

## `defer`

See [Errors and faults](errors.md#defer).

## Statements

```
binding · fn declaration · guard · while · for · defer · return
break · continue · assignment · expression
```

A `return` followed by a newline, `;`, or `}` is a bare return. Otherwise it
returns the expression that follows.
