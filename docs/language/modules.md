# Modules

Each source file is a module.

## Imports

```eit
import app from "app"
import errors from "lib/errors"
```

- Imports are explicit. Nothing is in scope from another module unless it is
  imported.
- The import path is a plain string literal, with no interpolation.
- The **host** decides which paths can be imported and what they resolve to.
  A path names either a host module, described by a declaration file the host
  registers, or an Eitri source module the host's loader supplies. See
  [Embedding: host modules](../implementation/embedding.md#host-modules).
  A script can't reach the filesystem or the network through `import`.
- Imported names are accessed with a dot: `app.log(...)`,
  `errors.LoadError`.

Imports appear only at module level.

Import cycles are compile errors. See [Module instances](#module-instances).

Imports must come before every other item in the module. The compiler can
then build the import graph by reading only the top of each file.

## Exports

`pub` marks a module-level item as exported. Everything else is private to its
module.

```eit
pub struct Window { ... }
pub fn open(title: string) -> Window { ... }
pub enum Theme { ... }

fn helper() { ... }           // private
```

`pub` is allowed only on module-level declarations and bindings, and on
the fields, methods, and static functions of structs and classes. Members are
private to the module unless marked `pub`; see
[Visibility](structs-classes-enums.md#visibility).

### Exported bindings are `let`

```eit
// config.eit
pub let default_theme = Theme.Dark
pub let audio = Audio { volume: 80 }     // a class instance

pub var volume = 80                      // error: exported bindings must be let
```

An exported binding can't be reassigned, by its own module or by importers.
Importers can therefore treat it as **stable**: the compiler resolves it once,
and folds it into each use site when its initializer is a constant.

Module-level `var` is allowed, but it stays private. There are two explicit
ways to let importers change module state:

```eit
// Share an object. Its identity is visible, as with any class.
pub let audio = Audio { volume: 80 }
// importer: config.audio.volume = 90

// Or keep the state private and expose behavior.
var volume = 80
pub fn set_volume(v: i64) { volume = v }
pub fn get_volume() -> i64 { return volume }
```

A function-typed field in an exported class instance gives replaceable
behavior, which is how Eitri achieves Lua-style monkey patching:

```eit
pub let hooks = Hooks { format: default_format }
// importer: config.hooks.format = my_format
```

Why not `pub var`: a reassignable export is a shared mutable global with
unclear ownership. Importers could never fold, narrow, or cache it, so every
read would pay for a fresh load from another module. The two forms above
reach the same outcomes and make the sharing visible in the types.

## Module-level code

A module's top level may mix declarations, bindings, and statements. Top-level
statements run in order, and the module as a whole behaves as the body of an
implicit `Error!void` function whose result goes to the host. See
[Errors and faults](errors.md#top-level-code).

## Module instances

- **One instance per import path per runtime.** Every importer of `"config"`
  in a runtime shares the same instance. Separate runtimes share nothing.
- **Initialized once, in dependency order.** An imported module's top-level
  code runs before the top-level code of any module that imports it. The whole
  import graph is known when the script is checked, so the order is fixed
  before anything runs.
- **Import cycles are compile errors.** This keeps initialization order
  deterministic, and no module is ever seen partially initialized. Mutually
  dependent code can move its shared part into a third module, or pass
  functions in.
- **Initialization can fail.** A module's top level is an implicit
  `Error!void` body. If an imported module returns an error or faults, the
  importing script fails with that outcome and its trace.

## Initialization order

Top-level functions are hoisted, but a module binding exists only once its
declaration has run. The compiler rejects top-level code that could reach a
binding before that:

```eit
setup()                  // error: setup() reads `count`, which is initialized on line 2
var count = 0
fn setup() { count += 1 }
```

How the check works:

1. For each function and method in the module, the compiler works out which
   module bindings it may read or write. It follows calls within the module
   and includes the bodies of nested lambdas.
2. At each top-level statement, every function it calls may use only
   bindings that are already initialized at that point.
3. **Referencing a function as a value counts as calling it.** For example,
   `app.on_start(setup)` above `var count` is rejected, because the host might
   call `setup` immediately. As a result, calls through function values need
   no tracking: when a function value is created, everything it needs is
   initialized, and a module binding never becomes uninitialized again.
4. **Imports need no check.** Imported modules are fully initialized before
   their importers run, and import cycles are rejected.

The analysis covers one module, and its cost is stated and bounded in
[Compiler: module analyses](../implementation/compiler.md#module-analyses).
There is no runtime "initialized?" check, so reading a module binding costs
nothing extra.

## Local by default

Every name is declared in a scope: a block, a function, or a module. There is
no implicit global namespace like Lua's `_G`, and assigning an undeclared name
is a compile error. A module-level binding is visible only inside its module
unless it is exported with `pub`, and importers reach it through the module
name, as `config.theme`.

Everything Lua scripts do with globals can be done with module state; see
[Eitri and Lua](../lua.md#globals-and-modules).

## Placement rules

| Item | Allowed at |
|---|---|
| `import` | Module level only |
| `struct`, `class`, `enum`, `type`, attributes | Module level only |
| `fn` | Module level and inside blocks |
| `pub` | Module-level items and struct or class members; on bindings, only with `let` |
