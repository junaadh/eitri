# Example: an editor configuration

A configuration for a Neovim-like editor, written in Eitri. It has the size
and shape of a respectable real-world Neovim config, and it is the
**reference workload** for Eitri's performance goals; see
[Performance goals](../../implementation/performance.md#the-reference-workload).

| Module | Lines | What it does |
|---|---|---|
| [`init.eit`](init.eit) | 31 | Entry point: runs each module's setup, then compiles an optional `local.eit` |
| [`options.eit`](options.eit) | 66 | About 40 editor options, leader keys, per-filetype indentation |
| [`keymaps.eit`](keymaps.eit) | 105 | About 35 keymaps as a data table, several with closures |
| [`autocmds.eit`](autocmds.eit) | 67 | Nine autocommands with callbacks |
| [`plugins.eit`](plugins.eit) | 130 | Twelve plugin specs with dynamic option tables |
| [`language_servers.eit`](language_servers.eit) | 104 | Six language servers with `dyn` settings, and buffer-local keymaps on attach |
| [`statusline.eit`](statusline.eit) | 52 | A statusline function the editor calls on every redraw |
| [`theme.eit`](theme.eit) | 41 | An enum with methods choosing the colorscheme by time of day |
| [`util.eit`](util.eit) | 39 | Shared helpers |
| **Total** | **635** | About 20 KB of source |

## Features exercised

- **Data:** structs with defaults, struct arrays as tables, maps, enum
  shorthand, `dyn` literals for plugin and server options
- **Functions:** closures capturing locals and host objects, function values,
  labeled and defaulted arguments, function-typed returns
- **Control flow:** `match` with range patterns, `if let`, `if` as an
  expression, `for` over arrays and maps, early `continue`
- **Errors:** recovery (`else e => …`) for missing files, failed formatting,
  and a broken `local.eit`
- **Strings:** interpolation, raw strings for editor regexes, `join`, `split`,
  iteration by `char`
- **Modules:** nine modules, imports, `pub` functions and constants
- **Runtime:** `compiler.load` checked against an expected function type

## Host API

The editor registers three host modules. Their declaration files are in
[`host/`](host/):

| Module | Declaration file | Provides |
|---|---|---|
| `editor` | [`host/editor.eit`](host/editor.eit) | Options (as host-class properties), keymaps, autocommands, buffers, diagnostics |
| `lsp` | [`host/lsp.eit`](host/lsp.eit) | Starting language servers and LSP actions |
| `pkg` | [`host/pkg.eit`](host/pkg.eit) | Plugin specs and setup |

How these bind to the host is described in
[Embedding](../../implementation/embedding.md#host-modules). In particular:

- `editor.options` is a host object. Each of its fields is a **property**, so
  `o.tab_stop = 4` calls the editor's setter, which applies the option.
- `editor.map` and `editor.on` **retain** the callbacks they receive, since
  the editor calls them long after the setup script has finished.
- `AutocmdEvent`, `Position`, `Highlight`, and `pkg.Spec` are plain structs,
  copied across the boundary.
- `editor.warn` is written in Eitri inside the host module.

## Lua equivalent

The benchmark compares this configuration against an equivalent `init.lua`
tree doing the same work through the same host API. That Lua version is
written together with the benchmark harness.
