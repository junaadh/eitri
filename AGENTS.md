# Working on Eitri

Eitri is a small, typed, embeddable scripting language for immediate execution.
Keep the implementation lean, efficient, and easy to measure. The guiding rule
is: **resolve what is known, specialize what is learned, execute directly, and
make every recurring cost earn its place.**

## Scope and implementation order

- Read [docs/README.md](docs/README.md), the relevant specification chapters,
  and [docs/tasks.md](docs/tasks.md) before implementing a feature.
- Follow the task dependencies. Start with the M0 scalar source-to-result
  slice; establish the test infrastructure alongside the initial Rust package.
  Finish a working vertical slice before broadening language coverage.
- Treat settled language behavior as the contract. Resolve specification
  conflicts explicitly; do not invent semantics to simplify implementation.
  Reject unsupported features before any submitted script executes.
- Keep task IDs and completion checks current. Record actual checks and
  measurements, and keep documentation consistent with implemented behavior.
  Do not mark a task complete based on scaffolding alone.

## Lean implementation

- Keep layers separate: source/diagnostics, frontend, typed operations and
  bytecode, VM, embedding, and CLI. Give each layer an explicit interface and
  dependency direction; put independently implemented layers in workspace
  crates as they land. The CLI handles process/file I/O, never core semantics.
- No process-global or thread-local state: no singleton runtimes, registries,
  interners, allocators, caches, or hidden mutable services. Immutable constants
  and generated read-only tables are fine.
- The caller owns the context and passes it explicitly. Share state between
  layers through borrowed context or the relevant fields, with clear lifetimes;
  avoid a service locator or an unstructured bag of unrelated state.
- Keep the compiler/execution path free of locks. Exclusive context access
  uses `&mut`, not `Mutex`, `RwLock`, or atomic reference counting. Independent
  contexts must operate independently, including in parallel tests. Reentry
  must borrow the active context and preserve its budgets rather than acquire
  a lock or create hidden state. Do not add atomics or claim a concurrent
  lock-free algorithm when ordinary ownership is sufficient.
- Use a Cargo workspace with `members = ["crates/*"]`. Start with the `eitri`
  package and focused modules. Add crates, dependencies, abstraction layers,
  and generated machinery only for a concrete need.
- Prefer direct, readable code and explicit ownership and data flow. Avoid
  speculative frameworks, duplicate representations, and unnecessary copies.
- Make allocations and retained memory deliberate. Reuse compiler scratch
  storage where useful; keep cold diagnostics and metadata off hot paths.
- Resolve names, types, offsets, and call targets once where the semantics
  permit. Do not introduce recurring dynamic work for statically known facts.
- State the complexity and resource bounds of module-wide algorithms. Exercise
  adversarial inputs as well as small examples.
- Preserve source order, observable host effects, faults, budgets, and GC/root
  contracts. Performance never justifies changing language semantics.
- `unsafe` is allowed when its performance benefit can be explained. State
  the cost it removes, why the safe alternative incurs that cost, and the
  measurements supporting it (or the experiment that will test it). Each
  block needs a `SAFETY` comment describing its invariants and why they hold.
  Keep unsafe operations explicit even inside unsafe functions, scope them
  narrowly, and validate affected code with Miri or sanitizers under T046.

## Performance is a design constraint

- Design for **sub-microsecond to a few microseconds from fresh source text to
  result for a small module in an initialized runtime**. Hot callbacks target
  under 1 microsecond. These are goals until measured, not achieved claims.
- Follow [docs/implementation/performance.md](docs/implementation/performance.md)
  for workload sizes, exclusions, and the proposed whole-config target.
  Compilation is part of source-to-result latency; a fast execution loop alone
  does not meet the goal. Never depend on a source cache or JIT warmup to claim it.
- Establish a reproducible baseline early. Measure compile, first execution,
  host-boundary overhead, and end-to-end latency separately. Account for
  allocations/bytes, scratch high-water, frames, code, and metadata size.
- Measure optimized builds with an observable, checked result. Use enough
  samples and batched measurements for timer overhead at nanosecond and
  microsecond scales; report the method and retain raw samples. Report median
  and p99 latency without presenting batch averages as individual-run tails.
- Record CPU, OS, pinned Rust/LLVM versions, build flags, workload, and exact
  command. Distinguish fresh modules from loaded dependencies. Report runtime
  creation, file I/O, and host work separately from the target operation.
- Compare equivalent work against Lua 5.4 as fixtures become executable.
  Include repeated compile/run/dispose workloads to expose memory growth.
- Profile the largest measured cost before adding an optimization. Compare
  before and after on identical workloads; retain negative results in the
  research ledger. Keep experimental execution paths optional until their
  promotion gates pass. Do not trade first-result latency for throughput
  without reporting both.
- Keep timing thresholds out of ordinary correctness tests. Performance
  regressions need controlled measurements, not flaky wall-clock assertions.

## Proper test harness from the start

- Build a reusable, deterministic fixture harness with the initial package
  (T045), rather than accumulating isolated example programs. Prefer Rust
  integration tests for direct compiler/runtime access. Python is acceptable
  for fixture orchestration, differential testing, and reporting when it
  makes the harness simpler; avoid maintaining two competing harnesses.
- Use one documented fixture format containing source, permitted host
  bindings, and expected outcomes: returned values, structured diagnostics
  with source ranges, faults with traces, ordered host effects, or budget
  exhaustion. Discover and run every fixture automatically.
- Give failures readable diffs and reproducible inputs. Keep fixtures
  independent so they run deterministically in parallel. Golden/snapshot
  updates must require an explicit command and review.
- For each implemented behavior, cover successful execution, rejection or
  fault behavior, and meaningful boundaries. Check unsupported neighboring
  features deliberately. Assert outcomes and semantics, not private structure.
- Provide the independent oracle hook from T004. Differentially compare the
  supported subset and run the same fixtures across execution tiers as they
  arrive; matching two paths that share the same bug is insufficient evidence.
- Add malformed-source and bytecode tests, arithmetic/encoding boundaries,
  execution and recursion limits, and repeated lifecycle checks as the
  relevant components land. Add fuzz entry points for compiler and verifier
  boundaries. Add forced-GC and host-reentry cases when those features exist.
- Separate correctness fixtures from benchmark fixtures, while sharing
  workload sources where useful. Benchmarks must validate their results too.
- Document exact build, format, test, fixture, and benchmark commands when
  bootstrapping the package. Keep the default correctness suite quick; expose
  stress, fuzz, sanitizer, and benchmark runs through explicit commands.
- Run formatting and relevant tests for code changes, and rerun the performance
  harness for compiler/runtime changes on the source-to-result path once it
  exists. Report unavailable checks plainly; never fabricate passing results.

## Review and completion

Keep changes focused and reviewable. Explain the resulting behavior, checks
run, and any measured performance or memory impact. A missed latency target
is useful evidence: record it and investigate the cost rather than weakening
the semantics or hiding work outside the measured operation.
