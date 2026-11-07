# Compiler

The compiler turns source text into bytecode in one short pass sequence. It is
designed for latency, not peak optimization: a tiny script should compile in
microseconds.

The [source-to-execution proposal](execution-design.md) describes how these
logical responsibilities can share traversals, starting with spelling IDs
and delimiter indexes in the lexer. The [research ledger](vm-research.md)
separates source-supported mechanisms from Eitri-specific layout hypotheses.
It does not authorize execution before checking finishes.

## Pipeline

```
source ─▶ lex ─▶ parse ─▶ resolve ─▶ check ─▶ allocate registers ─▶ emit ─▶ bytecode
```

| Stage | Does | Notes |
|---|---|---|
| Lex | Produces tokens, interning identifiers | Applies the [newline rules](../language/lexical-structure.md#newlines). Emits `>` singly |
| Parse | Builds the syntax tree in an arena | Recursive descent following [the grammar](../grammar.ebnf) |
| Resolve | Binds names to declarations | Collects top-level function signatures first, so forward calls work |
| Check | Type checks with local bidirectional inference | Exhaustiveness, narrowing, formatting checks, `mutating` rules |
| Allocate | Assigns frame slots to values | Reuses temporary slots; computes frame size |
| Emit | Writes bytecode and side tables | Light local optimizations only |

## Principles

- **Fully checked before running.** No code from a script runs until the whole
  script has been checked. Type errors never surface mid-execution.
- **Scratch memory is reused.** Arenas and buffers are reset between
  compilations instead of being freed and reallocated.
- **Light local optimizations only.** Constant folding, immediate operands, and
  choosing specialized instructions are in scope. Whole-program analysis is not.
- **Labels and defaults resolve statically.** Argument labels and default
  arguments are expanded at the call site, so calls cost the same as positional
  calls.

## Module analyses

Two checks need more than one function at a time: return-type inference
(which needs callees' return types first) and the
[initialization-order check](../language/modules.md#initialization-order)
(which needs to know which module bindings each function can reach). Both run
over one **module call graph**, built during name resolution.

| Step | Algorithm | Cost |
|---|---|---|
| Build the call graph | One edge per call to a function or static function in the same module, recorded while resolving names. Lambda bodies count as part of their enclosing function | O(calls) |
| Find recursive groups | Tarjan's strongly connected components | O(functions + calls) |
| Infer return types | Check groups in reverse topological order. A group with more than one function, or a self-call, must have annotated return types, so no fixpoint iteration is needed | Linear in the module |
| Binding reachability | One bitset of module bindings per group. Each group's set is its own direct reads and writes, OR the sets of the groups it calls, computed once in reverse topological order | O((functions + calls) × bindings / 64) |
| Check top-level code | For each top-level statement, test the called functions' bitsets against the bindings initialized so far | One bitset test per call |

Neither looks at other modules. Building the graph, finding groups, and
inferring return types are linear in the module. Binding reachability is
not: it costs O((functions + calls) × bindings / 64) time and
O(groups × bindings / 8) bytes, which grows **quadratically** when a module's
functions and bindings grow together.

Two things keep it small in practice:

- **Only late bindings are tracked.** A binding matters only if some
  top-level call happens before its declaration. Bindings declared before the
  first top-level call to a module function are always initialized when any
  function runs, so they get no bit. Typical modules declare their state and
  then call functions, so the bitsets are narrow.
- **Groups, not functions.** Each recursive group shares one bitset.

The stress workloads in [Performance goals](performance.md#workloads) include
a module where functions and late bindings grow together, so the quadratic
case is measured rather than assumed away.

## Parser notes

These are the places where the grammar needs more than one token of
lookahead or extra state.

| Situation | Approach |
|---|---|
| Lambda vs. parenthesized expression | Scan to the matching `)` and check whether `=>` follows |
| Array vs. map literal | Parse the first element; a following `:` means map |
| Error type in `E!T` | Parse a qualified name; if `!` follows, it was the error type |
| Struct literal in statement headers | Disallowed unparenthesized in `if`/`for`/`while`/`guard`/`match` headers, so `{` starts the body |
| `>>` in expressions | Two *adjacent* `>` tokens form a right shift |
| `>=` closing a type group | The type parser splits it into `>` and `=` |
| `try … else` vs. `guard … else` | After `try`'s operand, `else` is followed by a name or `_` and `=>` |
| Match arm separators | A comma, or newlines only after a block body |
| Match guard | Parsed as an `or_expr` so `=>` is never taken as a lambda |

## Diagnostics

Errors carry source locations. Debug metadata such as PC-to-line tables lives
outside the instruction stream, so it costs nothing on the hot path.

The compiler reports **several errors per run**. After an error, it recovers
at the next statement or declaration boundary and keeps checking, up to a cap.
Recovery only happens after an error, so valid scripts pay nothing for it.

Each diagnostic is printed with a source excerpt, in the layout of
codespan-reporting with rounded box-drawing characters:

```
error[E0204]: setup() reads `count`, which is initialized on line 14
   ╭─ init.eit:12:5
   │
12 │     setup()
   │     ^^^^^^^ called before `count` is initialized
13 │
14 │ var count = 0
   │ ------------- `count` is initialized here
```

Primary labels (`^`) mark the cause and secondary labels (`-`) give context.
A label spanning lines is drawn as a bracket (`╭ │ ╰`) left of the source,
with long spans shortened at a `·` break; labels in other modules get their
own `├─` section; trailing `= note:` and `= help:` lines carry advice.
Faults use the same form with `fault:` and no code, with the call trace as
labels, and broken internal invariants are reported as `bug:` rather than
panicking. Hosts can choose ASCII characters, a one-line `Short` form
(`init.eit:12:5: error[E0204]: ...`), or a `Medium` form with notes but no
excerpt.

The model and renderer live in the `eitri-diag` crate, shared by the
compiler, verifier, VM, and CLI. Producers record only byte spans; line
tables are built when a diagnostic is rendered, so valid scripts never pay
for them.

The host receives the same diagnostics as structured data: severity, code,
message, and source ranges.
