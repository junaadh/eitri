# Lexical structure

Source text is UTF-8. In this draft, identifiers are ASCII only. String
literals and comments may contain any UTF-8 text.

## Identifiers

```
IDENT = ( letter | "_" ) ( letter | digit | "_" )*
```

An identifier is never a keyword. A lone `_` is the wildcard token, not an
identifier, so it can't be used as a variable name.

### Keywords

```
async  await  break  class  continue  defer  else  enum  extend  false
fn  for  guard  if  import  in  interface  let  match  mutating
null  pub  return  self  spawn  static  struct  true  try  type  var  while
```

`yield` is reserved for future use (generators).

### Contextual words

These are ordinary identifiers except in one position:

| Word | Special only |
|---|---|
| `from` | directly after `import IDENT` |
| `map` | at the start of a type, as in `map[string]i64` |
| `task` | at the start of a type, as in `task void` |
| `weak` | at the start of a type, as in `weak Texture` |
| `chan` | at the start of a type, as in `chan Job` |
| `any` | at the start of a type, before an interface name, as in `any Shape` |
| `Self` | inside type and interface bodies, naming the type itself |

### Predeclared type names

`i8 i64 u8 u64 f32 f64 bool char string bytes void dyn` are names
predeclared in every scope, not keywords.

Predeclared type names can't be redeclared as types or used as variable,
function, or parameter names. Otherwise `let i64 = 5` would break `i64(x)`.

## Literals

### Integers

```eit
42
1_000_000
0xFF_FF
0o755
0b1010_0001
```

`_` separates digits anywhere after the first digit. An integer literal has no
type of its own: it takes its type from context, and the compiler rejects it
if the value does not fit that type. See [Types](types.md#integer-literals).

### Floats

```eit
3.14
1.0e-9
6e23
```

A digit must follow the `.`. That is why `0..n` lexes as `0`, `..`, `n` rather
than as a float. `1.` and `.5` are not float literals.

### Strings

```eit
"hello"
"tab\there"
"snowman: \u{2603}"
"Starting ${title}"
"literal \${not interpolated}"
```

| Escape | Produces |
|---|---|
| `\n` `\r` `\t` | newline, carriage return, tab |
| `\0` | NUL |
| `\\` `\"` | backslash, double quote |
| `\$` | a literal `$`, so `\${` produces `${` |
| `\u{…}` | the Unicode scalar with that hex value |

A `"…"` string can't contain a raw newline. Interpolation is covered in
[Strings and formatting](strings.md).

### Multi-line strings

```eit
let help = """
    Usage: tool [options]
      -v   verbose
    Version ${version}
    """
```

- The text starts on the line after the opening `"""` and ends on the line
  before the closing `"""`.
- The indentation of the closing `"""` is removed from every line, so the
  literal can be indented with the surrounding code.
- Escapes and interpolation work as in `"…"`.

### Raw strings

```eit
let pattern = r"\d+\.\d+"
let path = r"C:\games\save"
let block = r"""
    No \escapes or ${interpolation} here.
    """
```

`r"…"` and `r"""…"""` contain exactly what is written: no escapes and no
interpolation. A raw string can't contain the delimiter that ends it.

### Characters

```eit
'a'
'\n'
'\''
'\u{2603}'
```

A char literal is exactly one Unicode scalar between single quotes. It takes
the same escapes as strings, plus `\'`.

### Others

`true`, `false`, and `null` are keyword literals.

## Attributes

```eit
@extern
struct HostRect { ... }

@indirect enum List { ... }
```

An attribute is `@` immediately followed by a name. Attribute names are a
separate namespace, so they never clash with identifiers: `extern` and
`indirect` are ordinary names elsewhere.

Attributes come before `pub` and the declaration they apply to, on the same
line or on the lines above. The set of attributes is fixed by the language,
and an unknown attribute is a compile error.

| Attribute | Applies to | Meaning |
|---|---|---|
| `@extern` | `struct`, or a `class` in a host module | C layout, checked against the host's type. See [Extern structs](structs-classes-enums.md#extern-structs) |
| `@indirect` | `enum`, enum case | Payloads are stored in a heap box. See [Recursive types](structs-classes-enums.md#recursive-types) |

**Keyword or attribute?** Keywords change how code is typed or called at use
sites: `async`, `mutating`, `static`, `pub`. Attributes change representation
or host binding, and never change how the declaration is used.

The form `@name(arguments)` is reserved. No current attribute takes
arguments.

## Comments

```eit
// to end of line
/* block comment; does not nest */
```

Comments are discarded by the lexer.

## Operators and punctuation

The lexer uses longest match. Multi-character tokens are:

```
?.  ??  =>  ->  ..  ..=  ==  !=  <=  >=  &&  ||  <<
&+  &-  &*  +=  -=  *=  /=  %=  &=  |=  ^=  <<=  &+=  &-=  &*=
try?
```

Two special cases:

- **`try?`** is a single token only when `?` immediately follows `try` with no
  space.
- **`>` is always emitted singly.** The lexer never produces `>>` or `>>=`.
  This lets nested type groups like `[]<[]<E!T>>` close without special
  handling. The expression parser treats two *adjacent* `>` tokens as a right
  shift, and `>` followed by an adjacent `>=` as `>>=`. When the type parser
  meets a `>=` while closing a group, it splits it.

## Newlines

Braces delimit blocks, and a newline ends a complete statement. A semicolon can
separate statements on one line.

The lexer emits a `NEWLINE` token at a line break only when **both** hold:

1. The innermost open delimiter is `{`, or there is none. Inside `(` or `[`,
   line breaks are ignored.
2. The last token on the line is not a *continuation token*:

   ```
   ,  =  =>  ->  .  ?.  :
   ||  &&  ??  ..  ..=  ==  !=  <=  >=
   +  -  *  /  %  &+  &-  &*  |  ^  &  <<
   any compound assignment operator
   try  try?  await  spawn
   ```

Consecutive newlines count as one.

### Consequences

A line that ends in an operator continues:

```eit
let total = base +
    bonus
```

A line that *starts* with `.` does not join the previous line, because the rule
only looks backwards. Multi-line method chains need parentheses:

```eit
let cleaned = (input
    .trim()
    .lowercase())
```

`<` and `>` are not continuation tokens, because a type such as `?<E!T>` can
legitimately end a line. A comparison split across lines needs parentheses:

```eit
let fits = (width
    < max_width)
```

`else` must be on the same line as the `}` that precedes it:

```eit
if ready {
    start()
} else {          // OK
    wait()
}
```
