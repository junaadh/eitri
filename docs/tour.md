# A tour of Eitri

This chapter walks through one complete script. Each section links to the
reference chapter that defines the feature precisely.

## The program

```eit
import app from "app"

pub class Window {
    title: string
    width: i64
    height: i64
    parent: ?Window = null
}

enum Theme {
    Light
    Dark
    Custom(color: string)
}

enum LoadError {
    NotFound(path: string)
    Invalid(message: string)
}

fn find_window(windows: []Window, title: string) -> ?Window {
    for window in windows {
        if window.title == title {
            return window
        }
    }
    return null
}

fn load_theme(path: string) -> LoadError!Theme {
    let text = try app.read_file(path) else _ => .NotFound(path: path)
    let name = text.trim()
    guard name != "" else {
        return .Err(.Invalid(message: "empty theme file"))
    }
    return match name {
        "light" => .Ok(.Light),
        "dark" => .Ok(.Dark),
        _ => .Ok(.Custom(color: name)),
    }
}

let windows: []Window = [
    Window { title: "Main", width: 1200, height: 800 },
]

let shortcuts: map[string]string = ["save": "Ctrl+S", "quit": "Ctrl+Q"]

let title = find_window(windows, "Main")?.title ?? "Untitled"

let theme = match load_theme("theme.txt") {
    .Ok(theme) => theme,
    .Err(error) => {
        app.log("theme: ${error}")
        if app.dark_mode { Theme.Dark } else { Theme.Light }
    }
}

match theme {
    .Light => app.set_background("#ffffff"),
    .Dark => app.set_background("#111111"),
    .Custom(color) => app.set_background(color),
}

app.on_start(() => {
    app.log("Starting ${title}")
})
```

> `trim()` returns a plain `string`, because trimming can't fail. Rejecting an
> empty name is this caller's validation, so it's an explicit `guard`. See
> [Choosing `E!T`, `?T`, or a plain value](language/errors.md#choosing-et-t-or-a-plain-value).

## Walking through it

### Imports

```eit
import app from "app"
```

`app` is a host module. The host decides which module names a script may
import; nothing is available implicitly. → [Modules](language/modules.md)

### A class with a default field

```eit
pub class Window {
    title: string
    width: i64
    height: i64
    parent: ?Window = null
}
```

`Window` is a `class`: it has identity, and assigning a `Window` copies a
reference to the same object. It has to be a class because it refers to
itself through `parent`. A `struct` can't contain itself inline.
`pub` exports the type from this module. `parent` has a default, so a literal
can omit it.
→ [Structs, classes, and enums](language/structs-classes-enums.md#recursive-types)

### Enums with payloads

```eit
enum Theme {
    Light
    Dark
    Custom(color: string)
}
```

Enums are tagged unions. Cases are always PascalCase, and payload fields are
named.

### Returning an optional

```eit
fn find_window(windows: []Window, title: string) -> ?Window {
    for window in windows {
        if window.title == title {
            return window      // Window converts implicitly to ?Window
        }
    }
    return null
}
```

→ [Optionals](language/optionals.md)

### Returning a result

```eit
fn load_theme(path: string) -> LoadError!Theme {
    let text = try app.read_file(path) else _ => .NotFound(path: path)
```

`LoadError!Theme` is either a `LoadError` or a `Theme`. `try` unwraps the
success value of `app.read_file`, or returns early with an error. Because
`read_file`'s error type is not `LoadError`, the `else _ => ...` clause maps
it. The mapping runs only on failure. → [Errors and faults](language/errors.md)

```eit
    return match name {
        "light" => .Ok(.Light),
        "dark" => .Ok(.Dark),
        _ => .Ok(.Custom(color: name)),
    }
```

`match` is an expression and must be exhaustive. The `_` arm covers every other
string. `.Ok(.Light)` uses enum shorthand twice: the expected type
`LoadError!Theme` tells the compiler what `.Ok` and `.Light` mean.
→ [Pattern matching](language/pattern-matching.md)

### Collections

```eit
let windows: []Window = [
    Window { title: "Main", width: 1200, height: 800 },
]
let shortcuts: map[string]string = ["save": "Ctrl+S", "quit": "Ctrl+Q"]
```

The newline inside `[ ... ]` does not end the statement.
→ [Collections](language/collections.md), [Lexical structure](language/lexical-structure.md#newlines)

### Optional chaining and fallback

```eit
let title = find_window(windows, "Main")?.title ?? "Untitled"
```

If `find_window` returns `null`, `?.title` short-circuits to `null`, and `??`
supplies `"Untitled"`. `title` has type `string`.

### Handling an error with `match`

```eit
let theme = match load_theme("theme.txt") {
    .Ok(theme) => theme,
    .Err(error) => {
        app.log("theme: ${error}")
        if app.dark_mode { Theme.Dark } else { Theme.Light }
    }
}
```

A block's value is its final expression, so the `.Err` arm produces a `Theme`.
`${error}` works because enums whose payloads are formattable format as their
case name and payload. → [Strings and formatting](language/strings.md)

### Lambdas

```eit
app.on_start(() => {
    app.log("Starting ${title}")
})
```

The lambda captures `title`. → [Bindings and functions](language/bindings-and-functions.md#lambdas)

### The module's own result

Top-level code behaves as the body of an implicit `Error!void` function. If the
script runs to the end, the host receives success. A top-level `try` whose
error type is `Error` would hand that error to the host instead.
