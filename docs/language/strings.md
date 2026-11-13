# Strings and formatting

The current T015-3 implementation supports single-line and raw single-line
literals, local bindings, script function parameters and returns, and `.len()`
across control flow and calls, including host function arguments and results.
Other string operations, interpolation, multiline literals, and string values
in host-initiated script calls remain unsupported. See the
[implemented slice](../implementation/first-slice.md#managed-strings-t015-1-through-t015-3).
The rest of this chapter is the language contract for subsequent tasks.

## Interpolation

```eit
app.log("Starting ${title}")
app.log("size: ${width}x${height}")
app.log("next: ${count + 1}")
```

Any expression may appear inside `${ }`. To write a literal `${`, escape the
dollar sign: `"\${not interpolated}"`.

## What can be formatted

Formatting is checked at **compile time**. An interpolated value must have a
formattable type:

| Type | Formats as |
|---|---|
| `string` | Itself |
| `char` | Itself |
| Integers and floats | Their decimal value |
| `bool` | `true` or `false` |
| Enums | The case name, plus the payload if every payload field is formattable |
| Structs and classes | The result of their `describe() -> string` method |
| `dyn` | The value it holds, or `null` |
| `?T` | **Not formattable**. Use `${x ?? default}` or `match` |

```eit
struct Vec2 {
    x: f32
    y: f32

    fn describe() -> string {
        return "(${x}, ${y})"
    }
}

app.log("at ${position}")      // calls position.describe()
```

A struct or class without `describe()` can't be interpolated.

### Exact output

| Value | Text |
|---|---|
| A string or `char`, at the top level | Itself, unquoted: `theme.txt` |
| A string or `char`, nested in an enum, array, or map | Quoted: `"theme.txt"`, `'a'` |
| Enum case | `Dark`, or `NotFound(path: "theme.txt")` |
| Float | The shortest text that reads back as the same value; integral values keep `.0`: `0.1`, `2.0`, `1e+21` |
| Array | `[1, 2, 3]`, if its elements are formattable |
| Map | `["a": 1, "b": 2]` in insertion order, if keys and values are formattable |

## Cost

Formatting can allocate, and through `describe()` it can run user code. It is
never treated as free by the compiler or by execution budgets.

## Encoding

A `string` is immutable and always **valid UTF-8**. Arbitrary binary data
goes in [`bytes`](#bytes).

Text is validated only where it enters a script:

- Source code and literals are UTF-8 already.
- `string.from_utf8(b)` turns `bytes` into text, returning `Utf8Error!string`.
- Strings from the host are validated when they cross into the script,
  unless the host promises they are already valid.

## Characters: `char`

`char` is one Unicode scalar value. Char literals use single quotes and the
same escapes as strings:

```eit
let a = 'a'
let newline = '\n'
let snowman = '\u{2603}'

match c {
    'a' | 'e' | 'i' | 'o' | 'u' => vowels += 1,
    _ => {},
}
```

## Lengths, access, and iteration

| Operation | Returns | Cost |
|---|---|---|
| `s.len()` | Length in bytes | O(1) |
| `s.char_count()` | Number of `char`s | O(n) |
| `s.is_empty()` | `bool` | O(1) |
| `s.byte_at(i)` | `u8`; faults if out of bounds | O(1) |
| `s.slice(start, end)` | The substring between two **byte offsets**; faults if an offset is out of bounds or splits a `char` | O(1) check |
| `s.slice(r)` | The same, with a range of byte offsets: `s.slice(0..3)` | O(1) check |
| `s[i]` | Not supported, since it would be ambiguous between bytes and `char`s | — |

```eit
for c in name {                 // each char
    ...
}

for i, c in name {              // byte offset and char
    if c == ':' {
        let key = name.slice(0, i)
    }
}
```

Iteration decodes UTF-8 as it goes, without allocating. The offset from the
two-name form can be passed straight to `slice`.

## Ordering

`<`, `<=`, `>`, and `>=` compare strings byte by byte. For UTF-8, that is the
same as comparing code points. The comparison is a plain memory compare that
doesn't depend on any locale. Locale-aware collation is a library concern.

`char` values are ordered by code point.

## Concatenation

`+` joins strings:

```eit
let full = first + " " + last
```

The compiler merges a chain of `+` into a **single allocation**, using the
same code as interpolation. To build a string in a loop, collect the parts in
a `[]string` and call `join`, which avoids copying the growing string on
every step:

```eit
var parts: []string = []
for item in items {
    parts.push(item.name)
}
let list = parts.join(", ")
```

## Methods

These are built into the compiler for now.

| Method | Returns |
|---|---|
| `contains(sub)`, `starts_with(prefix)`, `ends_with(suffix)` | `bool` |
| `find(sub)` | `?i64`: the byte offset of the first match |
| `split(sep)` | `[]string` |
| `trim()` | `string` |
| `replace(from, to)` | `string` |
| `to_upper()`, `to_lower()` | `string`, using simple Unicode case mapping |
| `to_bytes()` | `bytes` |
| `[]string` `.join(sep)` | `string` |
| `i64.parse(s)`, `f64.parse(s)` | `ParseError!i64`, `ParseError!f64` |

Lua-style patterns are planned as a library.

## Representation

Strings are immutable, so these choices are invisible to scripts:

- A slice may share its parent's buffer or copy it. Short slices should copy,
  so a small substring doesn't keep a large string alive.
- A string caches its content hash after first use. Literals are interned in
  the runtime's intern table when a chunk loads, so equal literals are the
  same object and compare by pointer. Eager interning of short strings, as
  Lua does, is an experiment. See
  [Identity and hashing](../implementation/identity.md).

## `bytes`

`bytes` is an immutable byte string, like a Lua string.

| Operation | Returns |
|---|---|
| `b[i]` | `u8`; faults if out of bounds |
| `b.len()` | `i64` |
| `b.slice(start, end)` | `bytes` |
| `a + b` | `bytes` |
| `a == b` | Compares content |

A mutable byte buffer is a `[]u8`. Convert between the two with
`bytes(buf)` and `b.to_buffer()`; both copy.
