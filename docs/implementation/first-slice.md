# The first supported slice (M0)

Status: the M0 contract below was frozen by T002. T013 extends it as
described in [Scalar extensions](#scalar-extensions-t013). The historical
M0 restrictions below apply before those extensions. This chapter fixes what the first executable
implementation accepts, what it rejects, and the resource rules it runs
under. It narrows the language; it never redefines it. Everything outside
the slice is rejected with a diagnostic before any part of the submitted
script runs, and widening the slice later must not change the result of any
program the slice already accepts.

The [fixture corpus](../../crates/eitri/tests/fixtures/) is the executable
form of this chapter. Fixtures for behavior that does not execute yet carry
a `pending` directive (see the [root README](../../README.md#fixtures)).

## Scalar extensions (T013)

The current implementation additionally accepts `i8`, `u8`, `u64`, `f32`,
`f64`, and `char` in source and host signatures. Integer and decimal float
literals use their expected numeric type; defaults remain `i64` and `f64`.
Character literals contain one Unicode scalar, including supported escapes.
The [Types conversion rules](../language/types.md#explicit-numeric-conversions)
apply, including integer-only `truncating:` conversions. These intrinsic
conversions spend no call budget.

Arithmetic, comparisons, bitwise operators, shifts, and their compound
assignments follow [Expressions](../language/expressions.md). Arithmetic
operands have matching types; shift counts may have any integer type and do
not determine the left operand's type. Float comparisons retain IEEE NaN
behavior even in negated conditions. Float `%` is IEEE remainder (nearest
quotient, ties to even), not truncating remainder.

Out-of-range checked integer operations/conversions fault with `integer
overflow`; invalid float-to-integer conversions fault with `numeric conversion
out of range`; bad shift counts fault with `invalid shift amount`. Faults
preserve prior effects and source traces. Storage-only extern names remain
unavailable as ordinary source types. The initial managed-string slice is
listed below; the remaining M0 exclusions are still future work.

## Managed strings (T015-1 through T015-5)

Single-line and raw single-line UTF-8 literals can be stored in inferred
locals and used with `.len()`, which returns their byte length as `i64`.
The parser accepts the documented escapes; interpolation and multiline literals
are rejected. String annotations, local bindings, script function parameters
and returns work across branches, loops, and script calls. Host functions can
borrow string arguments through `Call::string_arg` and copy UTF-8 results into
the managed heap through `Call::set_result_string`. A borrowed argument cannot
cross reentry; the host can fetch it again afterwards. `Call::call_values`
supports mixed scalar and string arguments and copies string results out of
host-initiated reentrant script calls. String concatenation chains use one
result allocation, and strings support content comparisons, `.len()` and
`.is_empty()`, `.slice(start, end)`, `.contains()`, `.starts_with()`,
`.ends_with()` and `.find()`. Find produces a restricted optional byte offset
that can be stored and read using lazy `??`; general optionals are still
unsupported. `string(value)` converts scalar numbers, booleans and characters
to managed text; single-line `${expression}` interpolation also supports strings
and these scalar types. Invalid slice ranges and UTF-8 boundaries fault with
`InvalidStringSlice`. No host effects occur for a rejected program.
Hosts may retain a string argument across calls and executions and
return it to a script; explicit release removes its collector root.

## Types

| Type | Where | Notes |
|---|---|---|
| `i64` | Everywhere a type is written | Two's complement, 64 bits |
| `bool` | Everywhere a type is written | |
| `void` | Function return types only | |

Every other type name, every type constructor, and every literal of another
type (floats, strings, chars, `null`) is rejected with `E0001`.

## Syntax

### Module

A module is a sequence of items separated by newlines or `;`:

- `import NAME from "PATH"`, only before every other item. `PATH` must name
  a host module the host registered for this run. The path is a plain string
  literal; it is the only string literal the slice accepts.
- `fn` declarations.
- `let` and `var` bindings.
- Statements.

`pub`, attributes, `struct`, `class`, `enum`, `interface`, `extend`, and
`type` are rejected with `E0001`.

### Functions

```eit
fn add(a: i64, b: i64) -> i64 {
    return a + b
}
```

- Only at module level. Nested `fn` is rejected with `E0001`.
- Every parameter has a type and the `->` return type is always written,
  including `-> void`. Return type inference, defaults, `async`, and type
  parameters are rejected with `E0001`.
- Names are unique among the module's functions (`E0202`).
- A non-`void` function must return a value on every path (`E0402`).
  `return` without a value is allowed only in a `void` function. A path
  ends in `return` when a statement on it is `return`, or an `if` whose
  every branch (including the `else`) does; a `while` never counts.
- Results come only from `return`. The language also lets a body's final
  expression be its result (`fn square(n: i64) { n * n }`); the slice
  rejects a final expression with a value in a non-`void` function with
  `E0001` ("function results without `return`") rather than defining it.
- Top-level functions may call each other in any order, including recursion
  and calls to functions declared later.
- A body may use its parameters and locals, the module's functions, and
  imported host modules. It may **not** use a module-level `let` or `var`
  (`E0001`). This removes initialization-order analysis from the slice; T022
  lifts the restriction together with the check from
  [Modules](../language/modules.md#initialization-order).

Host functions come from a host module, a declaration file with bodiless
`pub fn` declarations whose parameters and results use the slice's types.
Scripts call them as `NAME.function(arguments)`.

### Bindings and assignment

```eit
let limit: i64 = 10
var total = 0
total += limit
```

- `let` and `var` take a single name and always an initializer. The
  annotation is optional; without it the binding has the initializer's type,
  and an integer literal is `i64`. Destructuring is rejected with `E0001`.
- A binding is visible from the statement after it. Redeclaring a name
  shadows the earlier binding, in the same block or an inner one.
- Predeclared type names (`i64`, `bool`, `void`, and the others listed in
  [Lexical structure](../language/lexical-structure.md#predeclared-type-names))
  can't name a binding, parameter, function, or import (`E0203`).
- Assignment targets a `var` binding (`E0303` otherwise). The operators are
  `=`, `+=`, `-=`, `*=`, `/=`, `%=`, `&+=`, `&-=`, and `&*=`; each compound
  form means `x = x op value`, with the same faults. The bitwise compound
  operators are rejected with `E0001` until T013.

### Statements

`let`, `var`, assignment, `if`, `while`, `return`, and expression
statements. `return` is allowed only inside a function (`E0001` at top
level, until the top level can return an `Error`).

`for`, `guard`, `defer`, `match`, `break`, `continue`, labels, and
standalone block expressions are rejected with `E0001`.

### Expressions

| Level | Operators | Operand types | Result |
|---|---|---|---|
| 2 | `\|\|` | `bool` | `bool`, short-circuit |
| 3 | `&&` | `bool` | `bool`, short-circuit |
| 4 | `==` `!=` | both `i64` or both `bool` | `bool`; non-associative |
| 4 | `<` `<=` `>` `>=` | `i64` | `bool`; non-associative |
| 11 | `+` `-` | `i64` | checked |
| 11 | `&+` `&-` | `i64` | wrapping |
| 12 | `*` `/` `%` | `i64` | checked |
| 12 | `&*` | `i64` | wrapping |
| 13 | prefix `-` | `i64` | checked |
| 13 | prefix `!` | `bool` | |
| 14 | `f(…)`, `NAME.f(…)` | | |

Also integer literals (all four bases, with `_`), `true`, `false`, names,
parentheses, and `if` expressions. `a < b < c` is a syntax error (`E0102`).
The other operators in [Expressions](../language/expressions.md) — `??`,
ranges, bitwise operators, shifts, `~`, `try`, `await`, `spawn`, lambdas,
`?.`, and indexing — are rejected with `E0001`.

**Calls.** Arguments are positional and are evaluated left to right. The
argument count must match (`E0304`). Labeled arguments are rejected with
`E0001`. A function name is usable only as a callee; using it as a value is
rejected with `E0001` (function values arrive with closures).

**`if`.** The condition is a `bool` (`E0301` otherwise). Used as a value, an
`if` needs an `else` and every branch must end in an expression of the same
type (`E0401`); the value of a branch block is its final expression
statement. Used as a statement, branches may be any blocks.

**Integer literals.** A literal must fit `i64` (`E0302`). A prefix `-`
applied directly to a literal forms one negative constant, so
`-9_223_372_036_854_775_808` is accepted and `9_223_372_036_854_775_808` is
not.

**Constant operands are not compile errors.** `9_223_372_036_854_775_807 + 1`
compiles, and faults when it executes, after every effect before it in source
order. Folding must preserve that.

## Top-level outcome

The module's top level is the implicit `Error!void` body of
[Top-level code](../language/errors.md#top-level-code). The slice has no
`Error` type, so a run has exactly three outcomes:

- **Rejected:** one or more diagnostics. Nothing runs, so no host function is
  called, even for top-level statements before the first error.
- **Success:** the top level completed. The host observes values only through
  the host functions the script called, in call order.
- **Fault:** execution stopped at a fault. Host calls made before it have
  happened. The fault has a kind, a message, and a trace.

### Traces

A trace lists the active frames, innermost first. Each entry names the
function, or `<module>` for top-level code, and the source range of the
operation that faulted (innermost) or of the call expression that is still
active (outer entries). A fault raised by a host function adds an innermost
entry with the host function's qualified name and no source range. A fault
in a nested run (see Host reentry) that the host propagates keeps the nested
run's entries, then the host function's entry, then the outer run's.

## Arithmetic

| Operation | Behavior |
|---|---|
| `+` `-` `*`, prefix `-` | Fault (`integer overflow`) if the exact result does not fit `i64` |
| `&+` `&-` `&*` | Two's complement wrap-around |
| `a / b` | Truncates toward zero. `b == 0` faults (`division by zero`); `MIN / -1` faults (`integer overflow`) |
| `a % b` | Sign of `a`. `b == 0` faults (`division by zero`); `MIN % -1` is `0` |

`MIN % -1` resolves a gap in the specification: the
[fault list](../language/errors.md#faults) names only `MIN / -1`, and the
remainder is exactly `0`, which fits.
[Division and remainder](../language/expressions.md#division-and-remainder)
records the same rule.

Operands are evaluated left to right, then the operation runs; a fault in
the left operand prevents evaluation of the right one. `&&` and `||`
evaluate their right operand only when needed.

## Resources

All limits belong to the caller's context and apply per run.

- **Execution budget.** A count of steps. Every call (script or host)
  debits one step before the callee starts, and every `while` iteration
  debits one step after its body completes, before the condition is
  evaluated again. When a debit finds no step left, the run faults with
  `execution budget exhausted`, at the call expression or the `while`
  keyword. A budget of `N` permits exactly `N` debits. The polls are
  defined by the source, not by the lowering: a rotated loop must still
  poll before re-evaluating its condition. The default is 1,000,000 steps.
  Nested runs (Host reentry, below) spend the same budget.
- **Call depth.** Script recursion uses VM frames, never Rust recursion. At
  most a configured number of script function frames may be active
  (top-level code is not counted). A call that would exceed it faults with
  `call depth exceeded` at the call, after the budget debit. The limit is
  counted in frames rather than bytes so the outcome is the same in every
  tier and frame layout; the default is 4,096. Byte accounting for frames
  joins the memory limit in T015.
- **Host reentry.** A host function may run a script function of the same
  program (not top-level code) through its call context. That call debits
  one step and counts as a frame, exactly like a script call: it faults
  with `execution budget exhausted` or `call depth exceeded` before the
  function starts. At most 64 nested runs may be active at once; one more
  faults with `call depth exceeded`. (Each nested run costs native stack:
  64 fit a default 2 MiB thread in every build.) Arguments that do not
  match the function's signature, including a `bool` other than 0 or 1,
  fault with ``host error: reentrant call to `f` does not match its
  signature``. A host names the function through a handle valid only
  during its own call (the Rust API's lifetimes enforce it), so it can
  only ever reenter the program it was called from. A fault in the nested
  run stops only the nested run and is
  returned to the host, which may handle it or propagate it to stop the
  outer run; its trace starts at the nested run's own frames.
- **Compilation limits.** Nesting deeper than 256 levels of blocks or
  expressions, more than 65,535 locals in one function, or source larger
  than 4 GiB is rejected with `E0002`, rather than exhausting the compiler's
  stack or index widths.

Every binding has an initializer, so no script-visible slot is read before
it is written; the verifier (T005) rejects code that would.

## Code ownership

A run compiles the submitted source into code owned by the context for the
duration of that run. The slice has no retained callbacks or exported
functions, so no code outlives its run: after `run` returns, its code,
constants, and metadata are released, and only reusable compiler scratch and
frame storage stay with the context. Repeated runs must not grow retained
memory (checked by T012).

## Diagnostic codes

<!-- BEGIN GENERATED DIAGNOSTIC CATALOG -->

| Code | Meaning |
|---|---|
| `E0001` | A language feature outside this slice. The message names the feature |
| `E0002` | An implementation limit from [Resources](#resources) |
| `E0003` | An artifact host index has a different binding in the destination registry |
| `E0004` | The runtime memory quota or allocator could not satisfy an allocation |
| `E0101` | Unexpected character, malformed literal, or unterminated comment or literal |
| `E0102` | Syntax error: expected one thing, found another |
| `E0201` | Unknown name |
| `E0202` | Duplicate function or import name |
| `E0203` | A predeclared type name used as a name |
| `E0205` | Unresolved import path, or `import` after another item |
| `E0301` | Type mismatch |
| `E0302` | Scalar literal does not fit its expected type |
| `E0303` | Assignment to something other than a `var` binding |
| `E0304` | Wrong number of arguments, or calling something that is not a function |
| `E0401` | `if` used as a value without `else`, or a branch without a value |
| `E0402` | Missing `return` in a function with a result |
| `E0901` | Malformed `.eits` assembly text |
| `E0902` | `.eits` that contradicts its program: an unknown or duplicate name, a mismatched type, slot, frame size, or encoding, or an unregistered host function |
| `E0903` | Bytecode the verifier rejects |

<!-- END GENERATED DIAGNOSTIC CATALOG -->

`E0204` is reserved for the initialization-order diagnostic in
[Compiler: diagnostics](compiler.md#diagnostics). The single source of
truth is the `Code` catalog in `crates/eitri-diag/src/codes.rs`. The marked
table is generated from `Code::ALL`; ordinary tests check it without writing.
To regenerate it explicitly, then review the diff:

```sh
cargo run --locked -p eitri-diag --example diagnostic_catalog -- --write
cargo run --locked -p eitri-diag --example diagnostic_catalog -- --check
```

## Error reporting and cascades

A run reports every independent error it finds, but one mistake gets one
diagnostic, including in half-typed code an editor saves mid-edit:

- Stages run in order and stop after the first one that reports an error:
  lexical errors suppress parsing, and syntax errors suppress checking, so
  one malformed token never turns into type errors.
- The lexer does not report a delimiter as unclosed when an unterminated
  literal or comment may have swallowed its closer: openers on the same
  line as an unterminated single-line literal, or anywhere before an
  unterminated multi-line one.
- The parser resynchronizes at the next statement boundary after a syntax
  error, and drops an unsupported construct whole. In a statement header,
  a `{` where an operand belongs is a missing operand, not a block.
- The checker treats anything already reported as an error type that is
  compatible with everything; reports an unknown name once per spelling,
  however often it is used; and, once an operator's left operand has the
  wrong type, checks the right one only for its own errors.

The `cascade/` fixtures hold these to account with mid-edit programs.
