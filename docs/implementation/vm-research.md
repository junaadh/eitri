# VM research and decision ledger

Status: research-backed proposal, 2026-11-07. Citations R1–R13 re-verified
2026-11-07, and instruction-set notes A1–A6 added: every link opened, metadata and quoted figures checked. No Eitri implementation results
exist yet. Read with [the execution design](execution-design.md) and
[the bytecode proposal](bytecode.md).

## Evidence rules

Research supports mechanisms under particular conditions. It does not prove
that a mechanism wins on Eitri, or select our operand widths, cache sizes,
fusion patterns, or tiering thresholds. Those are engineering hypotheses.
Each decision below has evidence, a limitation, and an experiment. Existing
language semantics remain constraints, not performance claims.

Compare total time to a result, not just steady-state throughput. Do not
import speedup numbers from another language, processor, or workload. Retain
negative results and report initialization, linking, memory, GC, and host ABI
costs rather than subtracting inconvenient parts of the chosen tier.

A citation supports investigating a mechanism, not adopting its
implementation rule. Hardware measurements hold for the cores and generated
instruction sequences measured, so the VM rules derived from them are
target-specific hypotheses until Eitri's own assembly is inspected and
timed. Sources quoted secondhand are marked *(secondary)* wherever a decision
cites them, are listed under [Not verified](#not-verified), and are never
the only evidence for a decision.

## Reading notes

### R1: Register versus stack execution

Shi, Casey, Ertl, Gregg, *Virtual Machine Showdown: Stack Versus Registers*,
ACM TACO, 2008. [Paper](https://www.cs.tufts.edu/comp/150FP/archive/david-gregg/vm-showdown.pdf).
See the abstract and experimental comparison of dispatch variants.

Evidence: register execution can reduce executed VM instructions, with larger
bytecode. The abstract reports eliminating "an average of more than 46% of
executed VM instructions, with the bytecode size of the register machine
being only 26% larger"; on AMD64 the speedup was 1.48× with switch dispatch
but only 1.15× with inline-threaded dispatch. Limitation: the study uses JVM implementations and older processors;
it does not measure Eitri's source-to-result workload. It motivates a register
candidate, not a universal ranking.

### R2: Bytecode has a serious alternative

Larose, Kaleba, Burchell, Marr, *AST vs. Bytecode: Interpreters in the Age of
Meta-Compilation*, OOPSLA 2023.
[Paper and repository record](https://kar.kent.ac.uk/102817/).

Evidence: AST interpreters were "on par with, or even slightly faster than
their bytecode counterparts" in their meta-compilation systems; bytecode was
more compact for larger applications, though it could use more memory for
small programs. Limitation: RPython and
GraalVM are different environments from a standalone Rust runtime. Keep a
compact typed-tree execution experiment for first-result latency.

### R3: A small native baseline

V8, *Liftoff: a new baseline compiler for WebAssembly*, 2018.
[Implementation account](https://v8.dev/blog/liftoff), sections on the new
pipeline and code generation. This is primary implementation evidence,
not a controlled Eitri experiment.

Evidence: a one-pass backend can track constants and value locations without
building a heavyweight optimizing IR. Limitation: WebAssembly has already
resolved much of the source-language frontend work. Compare the backend
only after charging Eitri's frontend and native-code publication costs.

### R4: Move backend work to runtime-build time

Xu and Kjolstad, *Copy-and-Patch Compilation*, OOPSLA 2021.
[Paper](https://fredrikbk.com/publications/copy-and-patch.pdf), especially
stencils, variant selection, and sections 6–7.

Evidence: precompiled binary fragments can be selected, copied, and patched
instead of running a conventional backend. Limitation: variant libraries
occupy space; in section 6 the code generation memory manager takes about
0.2 ms to initialize, a third of the 0.6 ms needed to generate one module,
and section 7's comparison amortizes LLVM's roughly 1 ms startup over 100
clones. Eitri must measure
its own code manager, including first use, rather than borrow headline ratios.

### R5: Generate the execution engine from operation semantics

Xu and Kjolstad, *Deegen: A JIT-Capable VM Generator for Dynamic Languages*.
[Reviewed version: arXiv v1, 2024](https://arxiv.org/html/2411.11469v1).
See sections 3.9, 6, and 7. Pin this version rather than assuming later
implementations have identical limitations.

Evidence: semantic descriptions can drive a specialized interpreter and
baseline JIT, including register pinning, variants, and cold paths.
Limitation: this version targets x86-64 and omits GC and Lua C bindings;
its benchmarks turn off LuaJIT's and PUC Lua's garbage collection too. Its performance does not establish
Eitri's embedding or AArch64 behavior. Adopting the method need not mean
adopting the whole toolchain.

### R6: Derive AOT code by specializing an interpreter

Fallin and Bernstein, *Partial Evaluation, Whole-Program Compilation*,
PLDI 2025. [Paper](https://cfallin.org/pubs/pldi2025_weval.pdf), sections on
context specialization and interpreter-state optimization.

Evidence: weval derives whole-function AOT code from an interpreter and
bytecode, without requiring execution traces. Limitation: its demonstrated
pipeline is not a drop-in sub-microsecond native Rust backend. Treat it as
an optional offline/AOT experiment; preserve synchronization points so
interpreter state can safely become externally observable.

### R7: Dispatch is not the only serial dependency

Ertl and Paysan, *The Performance Effects of Virtual-Machine Instruction
Pointer Updates*, ECOOP 2024.
[Paper](https://drops.dagstuhl.de/entities/document/10.4230/LIPIcs.ECOOP.2024.14);
[EuroForth 2024 slides](https://www.complang.tuwien.ac.at/anton/euroforth/ef24/papers/ertl-ip-updates.pdf).

Evidence: instruction-pointer updates can form a critical dependency chain;
combining updates can help. The slides report speedups above 2× on
loop-dominated benchmarks and 1.1–1.3× on call-dominated ones (Gforth, Tiger
Lake). Limitation: the experiments concern Forth and
particular CPUs. Measure dependency-aware fetch/fusion against a straightforward
decoder, not just branch-miss counts.

### R8: Canonical identity within a defined domain

Filliâtre and Conchon, *Type-Safe Modular Hash-Consing*, ML 2006.
[Primary abstract](https://gallium.inria.fr/ml2006/accepted/5.html).
Only the abstract and the authors' accompanying implementation description
were accessible in this review; no full-paper performance claims are used.

Evidence: canonicalization can share equivalent immutable terms and simplify
identity operations. Limitation: interning requires lookup work and a defined
equivalence/domain. Apply it selectively to names and type descriptions;
do not intern every AST node or treat a hash as proof of equality.

### R9: Allocate by compiler-object lifetime

Hanson, *Fast Allocation and Deallocation of Memory Based on Object Lifetimes*,
Software: Practice and Experience 20(1):5–12, 1990.
[Publisher abstract](https://onlinelibrary.wiley.com/doi/10.1002/spe.4380200104)
(blocks automated access); [Princeton TR-191-88](https://www.cs.princeton.edu/research/techreps/719).

Evidence: arena allocation groups objects with common lifetimes and releases
them together. Limitation: the accessible abstract supports the lifetime
mechanism, not our exact arena layout or reset policy. Separate compilation
scratch from executable artifacts; measure retained capacity and peak memory.

### R10: Cache dynamic lookup with complete guards

Hölzle, Chambers, Ungar, *Optimizing Dynamically-Typed Object-Oriented
Languages With Polymorphic Inline Caches*, ECOOP 1991.
[Paper](https://bibliography.selflanguage.org/_static/pics.pdf).

Evidence: per-site receiver caches reduce repeated dynamic lookup and can
also collect type feedback. Limitation: SELF message dispatch differs from
Eitri maps, visibility, and host properties. A guard must cover all facts
used by a cached recipe. Cache size and promotion policy need experiments.

### R11: SIMD structural scanning

Langdale and Lemire, *Parsing Gigabytes of JSON per Second*, VLDB Journal,
2019; [reviewed manuscript](https://arxiv.org/html/1902.08318v6), section 3.

Evidence: vector classification and structural indexes can reduce scanning
work. Limitation: JSON lacks Eitri's comments, contextual newlines, raw
strings, and nested interpolation. Experiment first with long comments,
strings, and whitespace; retain a scalar lexer and measure tiny-input cost.

### R12: Explicit state locations at observability points

LLVM, [Stack maps and patch points](https://llvm.org/docs/StackMaps.html).
Primary implementation documentation, not comparative research. Read alongside
R6's interpreter-state synchronization.

Evidence: native code can explicitly describe live value locations at chosen
program points. Limitation: stack maps are a mechanism, not a collector or a
complete correctness argument. Eitri must specify dynamic roots, suspended
frames, allocation-in-progress roots, and native-to-interpreter transitions.

### R13: Fusion has semantic boundaries

Ertl, Thalinger, Krall, *Superinstructions and Replication in the Cacao JVM
Interpreter*, Journal of .NET Technologies 4(1), 2006.
[Paper](https://www.complang.tuwien.ac.at/cd/papers/A73-full.pdf).

Evidence: combined handlers can remove dispatches (up to 4× on SpecJVM98),
while exception behavior and quickening complicate composition; the abstract
names "the conflict between superinstructions and the quickening
optimization". Replication had little effect on performance. Limitation: these JVM measurements
do not select Eitri's patterns. Fuse only with preserved fault locations,
budget polls, GC state, and evaluation order.

## Reading notes: dispatch and interpreters

### R14: The register VM Eitri follows most closely

Ierusalimschy, de Figueiredo, Celes, *The Implementation of Lua 5.0*,
J.UCS 11(7), 2005. [Paper](https://www.lua.org/doc/jucs05.pdf).

Evidence: the first widely used register VM, with 32-bit instructions and
cheap operand decoding by logical operations. A `sum` loop ran in 44% of Lua
4.0's time. Limitation: array-heavy gains of up to 40% include a new table
representation, not only the VM change.

### R15: Dispatch as a first-order cost

Ertl and Gregg, *The Structure and Performance of Efficient Interpreters*,
JILP 5, 2003. [Paper](https://jilp.org/vol5/v5paper12.pdf).
Casey, Ertl, Gregg, *Optimizing Indirect Branch Prediction Accuracy in
Virtual Machine Interpreters*, ACM TOPLAS 29(6), 2007.
[Paper](https://mural.maynoothuniversity.ie/10188).

Evidence: indirect branches were 3.2–13% of executed instructions and took
over half the run time in several simulated configurations; branch target
buffers predicted only 2–50% of dispatches. Replication plus superinstructions
gave up to 4.55× over threaded code. Limitation: simulated or BTB-era
predictors; see R16 and R17.

### R16: Pessimistic: modern predictors erase most dispatch cost

Rohou, Swamy, Seznec, *Branch Prediction and the Performance of
Interpreters — Don't Trust Folklore*, CGO 2015, pp. 103–114
(DOI 10.1109/CGO.2015.7054191, metadata confirmed via Crossref).
**Full text not verified**: the publisher and HAL pages blocked access. The
figures come from a [python-dev post quoting the paper](https://mail.python.org/pipermail/python-dev/2015-August/141172.html):
under 0.5 MPKI with simulated TAGE/ITTAGE, and Python's switch dispatch at
12.8, 3.5, and 1.4 MPKI on Nehalem, Sandy Bridge, and Haswell. A
[reply](https://mail.python.org/pipermail/python-dev/2015-August/141183.html)
notes that ARM predictors of the time were weaker.

Zakirov et al., 2010 ([paper](https://static.csg.ci.i.u-tokyo.ac.jp/papers/10/zakirov-ipsjpro2010.pdf)),
on a Core 2: total dispatch overhead of 0.6–3%; superinstructions that only
remove jumps improved performance "only marginally".

Challenges: effort spent on threaded dispatch and jump-removing fusion.

### R17: Counterpoint: fusion still pays on the same hardware

Ertl, [comp.compilers, 23 Oct 2022](https://compilers.iecc.com/comparch/article/22-10-056):
on Haswell with Gforth, replication gave 1.06–1.81×, dynamic superinstructions
1.18–1.96×, and both 1.67–3.42×. Miedema's 2020 thesis
([PDF](https://essay.utwente.nl/fileshare/file/81408/Miedema_MA_EEMCS.pdf))
reports JVM superinstructions giving 33–46%, and quotes an earlier Cacao
result where superinstructions gave nothing because the larger interpreter
no longer fit the instruction cache (secondhand only).

Qualifies R16: fusion removes work beyond the dispatch branch (slot traffic,
IP updates; see R7), so it must be measured, not assumed either way.

### R18: Pessimistic: tail-call interpreter gains were partly a compiler bug

Elhage, [*Performance of the Python 3.14 tail-call interpreter*](https://blog.nelhage.com/post/cpython-tail-call/), 2025;
Ken Jin, [*I'm Sorry for Python's tail-calling interpreter results*](https://fidget-spinner.github.io/posts/apology-tail-call.html), 2025;
[Python 3.14 What's New](https://docs.python.org/3.14/whatsnew/3.14.html);
Ken Jin, [*No longer sorry*](https://fidget-spinner.github.io/posts/no-longer-sorry.html), December 2025.

Evidence: the announced 9–15% came largely from an LLVM 19 change that merged
332 dispatch jumps into 3 and slowed the baseline by about 10%. Against a fair
baseline the gain was about 1–5%. Modern Clang already duplicates dispatch
from a plain `switch`. The later post attributes the remaining wins (about 15%
with MSVC, about 5% on AArch64) mainly to resetting compiler heuristics in a
very large function, not to register use. Tail calls are more *robust*:
`musttail` fails the build instead of silently regressing.

Haberman, [*Parsing Protobuf at 2+GB/s*](https://blog.reverberate.org/2021/04/21/musttail-efficient-interpreters.html),
2021: tail calls keep interpreter state in registers, but every non-tail call
forces a frame and spills, and `musttail` is non-portable.

Qualifies D9: pin the host compiler version, inspect the generated code, and
choose tail-call dispatch for robustness if it is chosen at all.

### R19: Hand-written and register-cached interpreters

Pall, [lua-l, 15 Feb 2011](http://lua-users.org/lists/lua-l/2011-02/msg00742.html):
LuaJIT's interpreter is assembly because in a large switch interpreter
"standard register allocation heuristics fail at this scale" and slow paths
"kill the opportunities for the fast paths". Its
[`vm_x64.dasc`](https://github.com/LuaJIT/LuaJIT/blob/v2.1/src/vm_x64.dasc)
pins `BASE`, `KBASE`, `PC`, and `DISPATCH` to fixed registers ("very
fragile… Caveat emptor") and notes replicated dispatch costs I-cache.

[wasm3's interpreter design](https://github.com/wasm3/wasm3/blob/main/docs/Interpreter.md):
converts stack bytecode to register form and uses tail-calling operations
with no dispatch loop, keeping two values in machine registers; self-reported
4–15× slower than native. Its cost: with one cached register an opcode maps
to "up to 3 different operations", with two to 10, so "the operation space
increases exponentially"; an extra float register "marginally" helped.

Supports: keeping PC, frame base, and constant base in host registers, with
cold paths out of line. Eitri tries this in portable Rust first (D9), and
checks the disassembly for spills.

### R20: Interpreters win short runs; untagged values pay

Titzer, *A Fast In-Place Interpreter for WebAssembly*, PACMPL OOPSLA 2022.
[Paper](https://arxiv.org/abs/2205.01183).

Evidence: removing value tags made the interpreter 8% faster on average (15%
at most). Threaded dispatch was 14% faster on average; handler alignment made
no significant difference. Baseline compilers took about 10× longer to
translate than the rewriting-free interpreter, and the interpreter was fastest
on short-running programs.

Supports untagged scalars and an interpreter-first path. Challenges an early
native tier for short setup scripts (D14).

## Reading notes: JIT principles

### R21: Pessimistic: warmup is unreliable

Barrett, Bolz-Tereick, Killick, Mount, Tratt, *Virtual Machine Warmup Blows
Hot and Cold*, PACMPL OOPSLA 2017. [Paper](https://arxiv.org/abs/1602.00602).

Evidence: at most 43.5% of (VM, benchmark) pairs consistently reached a steady
state of peak performance. Supports Eitri's "no warmup" rule: performance
must not depend on profiling or tier-up.

### R22: Quickening and inline caches

Brunthaler, *Efficient Interpretation using Quickening*, DLS 2010
([paper](https://www.unibw.de/ucsrl/pubs/dls10.pdf)): up to 2.18× with
threaded code. Brunthaler, *Inline Caching Meets Quickening*, ECOOP 2010
([paper](https://www.unibw.de/ucsrl/pubs/ecoop10.pdf)): up to 1.92×.
[PEP 659](https://peps.python.org/pep-0659/): specialization gave 10–60%;
caches stored in the instruction stream were "faster and simpler" than a
separate array; deoptimization rewrites an instruction back to its adaptive
form; superinstructions contributed "a small but useful" share. Hölzle,
Chambers, Ungar (R10): polymorphic inline caches gave a median 11%, or 27%
with type feedback into recompilation.

Qualifies D11: these gains come from removing *dynamic* type dispatch, which
Eitri's static types already remove. Expect them only at `dyn` sites, host
values, and constant-key map lookups.

### R23: Lazy basic block versioning

Chevalier-Boisvert and Feeley, *Simple and Effective Type Check Removal
through Lazy Basic Block Versioning*, ECOOP 2015.
[Paper](https://arxiv.org/abs/1411.0352).

Evidence: 71% of type tests removed, up to 50% faster. Code growth was only
0.19% with a 5-version cap (worst case +15%), against 223% for eager
versioning. Supports lazy, capped specialization if `dyn`-heavy code ever
needs it.

### R24: Pessimistic: a copy-and-patch JIT that rarely beats its interpreter

Ken Jin, [*Reflections on 2 years of CPython's JIT Compiler*](https://fidget-spinner.github.io/posts/jit-reflections.html), 2025:
"The JIT is almost always slower than the interpreter if you use a modern
compiler" (x64, Python 3.13/3.14); the reported 2–9% gains were measured
against the tier-2 interpreter, not the regular one.

Challenges D14: a baseline native tier is not a default win. Eitri is
statically typed, so the evidence is weaker against it than against
CPython's, but E8 must include first-use code costs and compare against the
best interpreter build.

### R25: Baseline compilers and interpreter design in production

V8, [*Firing up the Ignition interpreter*](https://v8.dev/blog/ignition-interpreter), 2016:
a register machine with an implicit accumulator; bytecode 25–50% the size of
baseline machine code. V8, [*Sparkplug*](https://v8.dev/blog/sparkplug), 2021:
one linear pass over bytecode with no IR, reusing the interpreter's frame
layout; 5–10% better Speedometer score. Wasmtime
[Winch RFC](https://github.com/bytecodealliance/rfcs/blob/main/accepted/wasmtime-baseline-compilation.md):
baseline compilers about 15–20× faster to compile than optimizing ones, with
code 1.1–1.5× slower. Poletto and Sarkar, *Linear Scan Register Allocation*,
TOPLAS 21(5), 1999 (DOI 10.1145/330249.330250): much faster than graph
coloring with nearly as good code.

Supports D14's shape if a native tier is built (one pass, interpreter frame
layout, linear scan). Ignition's accumulator is an encoding alternative for
E3.

Deegen (R5) numbers, from the paper and its
[companion post](https://sillycross.github.io/2022/11/22/2022-11-22/): the
generated interpreter is 179% faster than PUC Lua and 31% faster than
LuaJIT's interpreter; tail calls plus a GHC-style calling convention fixed
C/C++ register allocation problems.

## Reading notes: CPU microarchitecture

Eitri's VM is not microcode, but its handlers run on CPUs whose front ends
and memory systems decide what an instruction costs. Sources: Agner Fog,
[*The microarchitecture of Intel, AMD and VIA CPUs*](https://www.agner.org/optimize/microarchitecture.pdf),
edition of 2026-05-23 (section numbers below); Intel 64 and IA-32
Optimization Reference Manual, Vol. 2 (earlier generations), 356477-050US,
via a [mirror](https://kib.kiev.ua/x86docs/Intel/Intel-OptimGuide/356477-050.pdf)
(the current Vol. 1 was not retrievable).

### M1: Macro-op fusion

Compare, test, and some arithmetic instructions fuse with the following
conditional jump into one compute-and-branch µop that executes "with a single
dispatch" (Intel §2.1.1.1). Since Ice Lake an instruction with a memory
operand doesn't fuse, and the pair must be adjacent (Agner §12.6). Zen 5
fuses CMP, TEST, ADD, SUB, AND, OR, XOR, INC, and DEC with all conditional
jumps, except with RIP-relative or displacement-plus-immediate operands
(§25.3).

For Eitri: a VM compare-and-branch should never write a boolean slot, and its
handler should compare two registers (`load a; load b; cmp; jcc`), not a
register against memory, so the host pair can fuse on cores that fuse it
(an expectation to confirm in the generated assembly, not a guarantee). A fused VM operation
must not span a safepoint or a source line (see the Faster CPython
[discussion](https://github.com/faster-cpython/ideas/issues/585) of
superinstructions breaking instrumentation and debugging).

### M2: µop cache and loop buffer

Decoded µop caches hold about 2,304 µops on Ice Lake (§12.3), 4,096 on Alder
Lake P-cores (Table 13.1), and 6,144 on Zen 5 (§25.4). The loop stream
detector can't hold CALL, RET, or more than 8 taken branches (Intel
§2.1.1.4), so a dispatch loop never runs from it; switching between the µop
cache and the legacy decoders has a penalty (§2.1.1.2).

For Eitri: the hot handlers' total size is a budget, measured on the
smallest-cache core targeted. Every new specialized or fused opcode spends
from it, so it must earn its place.

### M3: Indirect branch prediction

ITTAGE predicts indirect targets from tagged tables indexed by geometric
lengths of global path history (Seznec, [CBP-3, 2011](https://jilp.org/jwac-2/program/cbp3_07_seznec.pdf)).
Zen 4 tracks about 3,072 indirect targets with a 32-entry return stack
([Chips and Cheese](https://chipsandcheese.com/p/amds-zen-4-part-1-frontend-and-execution-engine));
Zen 5 has a 52-entry return stack (§25.6). This is the mechanism behind R16.

For Eitri: measure indirect-branch MPKI with the plain `match` dispatcher
before building threaded variants (D9), on both x86-64 and AArch64.

### M4: Register renaming and move elimination

Zen 5 has 240 integer physical registers and eliminates register-to-register
moves at rename, six per clock (§25.7, §25.13); zeroing idioms are
dependency-free (§25.14).

For Eitri: a VM `MOV` between slots costs a load, a store, and a dispatch;
nothing in software is free the way renaming is. The analog is compile-time
copy coalescing in the slot allocator, measured as the share of executed
moves.

### M5: Store-to-load forwarding

Frame slots live in memory, so every dependent pair of VM operations pays
store-to-load latency on the critical path. Forwarding takes 5 cycles at best
on Ice Lake, and a read wider than, or partly overlapping, the write fails to
forward and costs 19–20 cycles (§12.13). Zen 5 forwards general-purpose
registers in 2 cycles, with about 12 cycles for partial overlaps and up to 28
for some unaligned offset reads (§25.17); Zen 2–5 can mirror memory operands
with zero latency (§25.18). Travis Downs
[measured](https://gist.github.com/travisdowns/bc9af3a0aaca5399824cf182f85b9e9c)
split writes followed by a wider read at about 14–21 cycles on every CPU
tested.

For Eitri: always store slots at full width (sign- or zero-extending narrower
scalars), never read a slot wider than it was written (including in frame
copies and collector scans), and copy flattened structs slot by slot. The
same VM can rank differently on Intel and AMD, so both are measured.

### M6: Value prediction and its software analogs

VTAGE (Perais and Seznec, HPCA 2014) predicts values from global branch
history with confidence estimation (abstract only verified,
[record](https://www.scipedia.com/public/Perais_Seznec_2013a)). The software
analogs are quickening, inline caches, and basic block versioning (R22, R23).

For Eitri: cached sites first use per-runtime side tables, because code is
immutable and can be shared between runtimes. In-place quickening of a
private per-runtime copy is the experiment; equal instruction length keeps
PCs and stack maps valid, but correctness needs the enumerated transitions in
[the bytecode proposal](bytecode.md#caches-and-quickening).

### M7: Branches versus conditional moves

"A conditional jump is faster than a conditional move if the code is part of
a dependency chain and the prediction rate is better than 75%" (Agner,
[*Optimizing subroutines in assembly language*](https://www.agner.org/optimize/optimizing_assembly.pdf), §9).

For Eitri: overflow, bounds, and divide-by-zero checks stay as predictable
never-taken branches to cold code; compare-and-branch computes the next PC
with a real branch, not a conditional move, so the next dispatch isn't
delayed behind the compare.

## Reading notes: instruction-set design

These notes back the AArch64-style discipline in
[the bytecode proposal](bytecode.md#design-discipline). They describe what
hardware ISAs chose and why; an interpreter pays for decoding where a CPU
does not, which is why D28 separates dispatch fields from operand fields.

### A1: AArch64 families, fields, and aliases

AArch64 encodes one operation with fields instead of separate opcodes: `sf`
selects 32- or 64-bit registers, and condition, shift, and extend are fields.
Many mnemonics are aliases of one encoding: `LSL`, `LSR`, `UBFX`, `UXTB`,
and `UXTH` are all `UBFM` ("unsigned bitfield move"). There is no byte or
halfword arithmetic; narrow values use full-register operations and these
extends. `CSEL` "writes the value of the first source register to the
destination register if the condition is TRUE", and sets no flags. Source:
the A64 instruction pages generated from Arm's XML, 2026-03 release,
mirrored at [UBFM](https://www.scs.stanford.edu/~zyedidia/arm64/ubfm.html)
and [CSEL](https://www.scs.stanford.edu/~zyedidia/arm64/csel.html).

For Eitri: one handler per family template, aliases only in the
disassembler, and narrow integers as a 64-bit operation plus a narrowing
(D30, D31).

### A2: Compare-and-branch returns to AArch64

Armv9.6's FEAT_CMPBR adds `CB<cc>`, which "compares the values in two
registers, and conditionally branches" without touching the flags. Its
condition field is 3 bits with six values (`GT GE HI HS EQ NE`); compilers
express the other conditions by swapping registers or adjusting the
immediate. Sources: [CB&lt;cc&gt; (register)](https://www.scs.stanford.edu/~zyedidia/arm64/cbcc_regs.html),
[LLVM codegen PR #116465](https://lists.llvm.org/pipermail/llvm-commits/Week-of-Mon-20241111/1504420.html),
[binutils support](https://patchwork.sourceware.org/project/binutils/patch/20250415084943.12580-1-Ezra.Sitorus@arm.com/).

For Eitri: a 3-bit condition as a dispatch field, operand swap for the
register forms, and immediate adjustment for the immediate forms (D32).

### A3: Removing per-instruction predication

ARM32 could make almost any instruction conditional; A64 cannot. The
Armv8-A Programmer's Guide (§6.2.5) gives the reason: "Predicated execution
of instructions does not offer sufficient benefit to justify its significant
use of opcode space." Conditional select, increment, and compare remain.
Quoted secondhand: Arm's page did not load, and the text comes from a
[Stack Overflow answer](https://kiwix.lema.org/stackoverflow.com_en_all_2023-11/questions/65092001/what-s-aarch64-s-way-to-conditionally-load-store)
quoting it.

For Eitri: no guard field on any instruction; a select is measured, not
assumed (D33).

### A4: No condition codes

The RISC-V specification: "The conditional branches were designed to include
arithmetic comparison operations between two registers … rather than use
condition codes (x86, ARM, SPARC, PowerPC)", conceding an advantage to
condition codes only when several branches share one comparison, which it
believes "to be relatively rare" (2019 spec, p. 23, quoted in a
[mailing-list post](https://www.mail-archive.com/debian-italian@lists.debian.org/msg150775.html);
secondhand). RISC-V later added conditional operations in the
[Zicond extension](https://docs.riscv.org/reference/isa/v20240411/unpriv/zicond.html).

For Eitri: the same trade, for a stronger reason. An interpreter's flags
would live in memory between dispatches (M5) (D32).

### A5: Lua 5.4's encoding

Lua 5.4 uses a 7-bit opcode and spends the freed bit on `k`; "for
comparisons, k specifies what condition the test should accept". All
comparisons skip the next instruction, which is always a jump, and the
compare handler performs that jump itself (`donextjump`), so a condition
costs one dispatch and two words. There are 83 opcodes. Sources:
[lopcodes.h](https://www.lua.org/source/5.4/lopcodes.h.html),
[lvm.c](https://www.lua.org/source/5.4/lvm.c.html).

For Eitri: compare-and-branch in one word; register and immediate forms as
dispatch fields rather than a tested `k` bit, measured in E11.

### A6: Predecoding in the dispatch sequence

LuaJIT's x64 `ins_NEXT` loads the instruction, extracts the opcode and A
into registers, shifts D into place, and jumps through the dispatch table,
before any handler runs (`movzx RAd, RCH; movzx OP, RCL; add PC, 4; shr RCd,
16; jmp aword [DISPATCH+OP*8]`). This works because A is in the same place
in every format. Source:
[vm_x64.dasc](https://raw.githubusercontent.com/LuaJIT/LuaJIT/v2.1/src/vm_x64.dasc).

For Eitri: fixed field positions in every short format, and the generated
dispatch predecodes opcode and A (D29). Whether Rust's `match` does this is
checked in the disassembly (D24).

## Reading notes: emitted work

### R28: Deferred materialization

V8's Liftoff baseline compiler (Backes, [*Liftoff*](https://v8.dev/blog/liftoff),
2018) keeps a virtual stack of values in registers or spill slots; for a
constant it "only records the constant's value in the virtual stack and does
not generate any code. Only when the constant is used by a subsequent
operation, it is emitted or combined with the operation." Lua 5.4's
[`expdesc`](https://www.lua.org/source/5.4/lparser.h.html) does the same for
a register VM: `VKINT` is a constant, `VNONRELOC` "has its value in a fixed
register", and `VRELOC` "can put result in any register" (the instruction's
destination is patched later).

For Eitri: one operand descriptor in lowering, so constants become
immediates and results land in their final slot without a move (D37).
Limitation: Liftoff targets machine code and Lua is untyped; the saving in
Eitri's executed moves is measured in E3.

### R29: Sized constructors

Lua 5.4's [table constructor](https://www.lua.org/source/5.4/lparser.c.html)
counts record entries (`nh`) and array entries (`na`) while parsing, then
calls `luaK_settablesize(fs, pc, t->u.info, cc.na, cc.nh)` to size the
`OP_NEWTABLE` it emitted first. A trailing multi-result expression is not
counted.

For Eitri: literal sizes are exact in more cases, because types are known;
any element count that depends on run time is only a lower
bound (D40).

### R30: Generated superinstructions

[vmgen](https://www.complang.tuwien.ac.at/anton/vmgen/html-docs/Superinstructions.html)
generates a superinstruction from a declared sequence (`lit_sub = lit sub`)
wherever the sequence occurs, and its profiler helps choose sequences. An
instruction that changes the instruction pointer "should only be used as
last component"; vmgen does not check this, and violations "just result in
bugs". Its documentation says: "don't invest too much work in (static)
superinstructions."

For Eitri: candidates come from the corpus (D42), the operation
specification checks the composition rules mechanically, and R16–R17 decide
how much fusion is worth on current cores.

## Reading notes: collection and generics

### R26: Conservative stack scanning

Shahriyar, Blackburn, McKinley, *Fast Conservative Garbage Collection*,
OOPSLA 2014. [Paper](https://www.cs.utexas.edu/~mckinley/papers/conservative-gc-oopsla-2014.pdf).

Evidence: scanning stacks and registers conservatively falsely retained under
0.01% of objects and pinned 0.03%; conservative Immix and reference counting
came within 2–3% of their exact versions. The large costs belonged to older
collector designs, not conservatism itself.

Challenges D12: the first non-moving collector could scan frames
conservatively and skip stack maps and split unions entirely. Limitation:
Eitri's untagged `i64` slots can look like pointers more often than Java's
typed stack slots, so false retention must be measured on Eitri's workloads.

Blackburn and McKinley, *Immix*, PLDI 2008
([paper](https://www.cs.williams.edu/~dbarowy/cs334s18/assets/immix-pldi-2008.pdf)):
7–25% better total performance on average. Zhao, Blackburn, McKinley, *LXR*,
PLDI 2022 ([paper](https://arxiv.org/abs/2210.17175)): 4% better throughput
than G1 with much lower tail latency. Both are candidates after mark-sweep;
Immix's line-level pinning fits the hash-state plan in
[Identity and hashing](identity.md#experiments).

### R27: Specialization versus shared generic code

Kennedy and Syme, *Design and Implementation of Generics for the .NET CLR*,
PLDI 2001 ([paper](https://web.eecs.umich.edu/~bchandra/courses/papers/Kennedy_Generics.pdf)):
code is shared between instantiations with the same GC pointer pattern, and
polymorphic recursion is supported by specializing lazily at run time. Go's
[GC-shape stenciling design](https://github.com/golang/proposal/blob/master/design/generics-implementation-gcshape.md)
uses the same idea, and lists lost inlining and unresolved method calls as
risks; [PlanetScale measured](https://planetscale.com/blog/generics-can-make-your-go-code-slower)
a generic call through a pointer about 42% slower than a monomorphized one.
Ellis, Zhu, Yoshida, Song, *Generic Go to Go*, PACMPL OOPSLA 2022
([paper](https://arxiv.org/abs/2208.06810)): monomorphization runs faster, but
its compile time and code size can grow far faster than the program.
Anderson ([Rust internals, 2015](https://internals.rust-lang.org/t/some-notes-on-reducing-monomorphizations/2459)):
merging about 10–13% of duplicate instantiations didn't reduce compile time.

Supports the instantiation cap and treating shape sharing as an experiment
with small expected gains. Qualifies the ban on polymorphic recursion: it
follows from compiling everything before running, not from necessity, and
could be lifted later by lazy instantiation.

## Experiment results

### T064 M0 security review — 2026-11-12

The [security audit](security.md) retains the scalar execution design and
records its trust/denial-of-service limits. No execution-path optimization
was attempted: removing safety checks or inventing memory-limit semantics
would not be justified by this review. New adversarial tests cover long
tokens, syntax depth, assembly cycles across both tiers/interpreters, and
caught reentry faults sharing fuel. Memory/compiler-work quotas, hash-flood
resistance, verifier state bounds, and stronger host/artifact validation are
deferred explicitly. Existing E2/E5 unsafe mechanisms were inspected and
checked with Miri; no unsafe was added. This review makes no new latency or
allocation improvement claim. Commands, results and limitations are recorded
against T064 and in `/tmp/codex-eitri-par-T064.md`.

### T011 baseline: source to result against Lua 5.4 — 2026-11-07

**Method.** `cargo bench -p eitri --bench source_to_result` and
`crates/eitri/benches/lua/run.sh`, run one after the other on an Apple M5
(10 cores), macOS 27.0.1. Eitri: Rust 1.97.1, LLVM 22.1.6, release profile,
system allocator, `match` dispatch (D9), no `unsafe` in the VM. Lua 5.4.9
from Homebrew, linked statically, Apple clang 21 `-O2`. Five workloads in
`crates/eitri/benches/fixtures/`, each written once in each language and
ending in one host call carrying a result checked on every run: `module` (a
40-line setup-script shape, about 1.2 KB), `arithmetic` (1,000 loop
iterations of checked and wrapping arithmetic), `branching` (1,000
iterations of `else if` chains, `&&`/`||`, and a call), `calls` (recursive
`fib(15)`, 1,973 calls), and `host_calls` (1,000 scalar host calls). Per
sample: compile (Eitri lex/parse/check/lower; Lua `luaL_loadbuffer`) and
first execution (`execute`; `lua_pcall`) are timed separately, and the
total is their sum. 20,001 individually timed samples per cell; the timer
is quantized to 41.7 ns. "Reused" is one initialized runtime warmed by
1,000 runs; "fresh" creates a runtime per sample, creation excluded and
reported on its own. Raw samples: `target/bench-results/*.csv`.

**Results (reused runtime, p50; p99 in the bench output).**

| Workload | Eitri compile | Lua compile | Eitri execute | Lua execute | Eitri total | Lua total |
|---|---|---|---|---|---|---|
| module | 6.4 µs | 8.9 µs | 0.21 µs | 0.21 µs | 6.6 µs | 9.2 µs |
| arithmetic | 1.7 µs | 2.1 µs | 18.8 µs | 15.0 µs | 20.5 µs | 17.2 µs |
| branching | 3.3 µs | 4.1 µs | 43.1 µs | 31.1 µs | 46.4 µs | 35.3 µs |
| calls | 1.3 µs | 1.5 µs | 27.0 µs | 14.3 µs | 28.2 µs | 15.8 µs |
| host_calls | 0.9 µs | 1.1 µs | 10.5 µs | 10.2 µs | 11.3 µs | 11.4 µs |

Fresh runtimes: creation is about 0.9 µs in both. Eitri's fresh compile
is 0.7–2.1 µs slower than its reused compile (scratch not yet grown);
Lua's differs by 0.1–0.2 µs.

Memory, per reused run: 11–55 allocations (0.7–4.8 KB), mostly the
compiled functions' boxed slices and diagnostics storage. The context
retains 7.8–24.7 KB of scratch after warm-up. Code is 52–364 bytes; the
metadata beside it (spans, start bits, sites, constants) is 4–5× larger.

**Reading.** Compilation is 1.2–1.4× faster than Lua's and the small
module is about 1.4× faster end to end, but it is 6.4 µs for 1.2 KB
(5.2 ns/byte), well above the frontend's 3.4 ns/byte: lowering, the
`Compiled` allocations, and cold paths need a profile. Execution loses
wherever the loop runs: 1.25× on arithmetic, 1.4× on branching, and 1.9×
on script calls (about 14 ns per call against Lua's 7). Host calls are on
par. The module target (sub-microsecond to a few microseconds) is missed:
6.6 µs. Next: profile the VM's dispatch and call path (E5, E4) and the
compile path beyond the frontend, before changing either.

### E4/E5 and T011: path profiles — 2026-11-07

**Scope and method.** Profiled the existing call and loop paths first, then
compilation beyond the frontend; no optimization or production-path changes.
Same Apple M5/macOS 27.0.1, Rust 1.97.1/LLVM 22.1.6, system allocator and
workspace release defaults as T011. The new `eitri` example `path_profile`
uses the T011 fixtures and known answers without the benchmark counting
allocator. Build with `CARGO_PROFILE_RELEASE_DEBUG=2 cargo build --release
-p eitri --example path_profile`; `xcrun dsymutil` provides macOS symbols.
The initial Instruments recordings used `CARGO_PROFILE_RELEASE_DEBUG=1`;
full symbols were then built for the `sample` recordings. VM instruction
addresses were checked against disassembly. No CPU affinity was imposed.
Exact commands and timing boundaries are in the
[benchmark README](../../crates/eitri/benches/README.md#cpu-profiles-and-compile-stages).

`vm calls`, `vm arithmetic`, and `vm branching` execute already-compiled
modules repeatedly, checking every result. Instruments Time Profiler recorded
5 seconds of work at 1 ms sampling; independent 5-second macOS `sample`
recordings supplied symbolicated stacks. The latter loop profiles skipped
startup by one second; the call profile included loader samples, which are
excluded from its 3,825 steady execution samples. `compile module` profiles
fresh compilation plus disposal; `lower module` builds the checked input once
and profiles lowering plus disposal. These isolate CPU costs and make no
fresh-source or first-execution latency claim.

**VM findings.** Among running Instruments samples, the shared instruction
head (image offsets `0x5700..0x57c4`, end exclusive) holds 2,798/5,004
samples for calls (55.9%), 4,173/4,990 for arithmetic (83.6%), and
3,877/4,998 for branching (77.6%). This includes fetching the instruction,
`WIDE` handling, opcode validation/mapping, frame slice checks, operand
extraction and the dispatch-table lookup/indirect branch. The hottest PC
is the dispatch-table load at `0x57b8`. Samples near a branch can include
stalls; these percentages do **not** isolate branch misprediction cost or
prove a particular dispatch replacement will win.

The symbolicated call profile has 493/3,825 samples (12.9%) in `memmove`
and its stubs: 245 argument-copy samples and 248 result-copy samples, mapped
by their return addresses to `CALL_SCRIPT` and `RET`. The branching profile
has 161/4,222 (3.8%) there. Arithmetic has 4,246/4,248 samples directly in
the VM and only two in startup-frame initialization/copy helpers. Call-frame
zeroing has only 5 call-profile samples; repeated allocation/frame growth
is not the dominant steady cost. Control-frame push/pop, range checks and
callee/caller restoration remain inside the interpreter's own samples.

**Reading:** shared instruction overhead is the first VM experiment to
measure; copying alone is unlikely to close the roughly 2× call gap. E4
must still report the total call/return path, rather than treating the
`memmove` percentage as its full cost. E5 needs an assembly-controlled
comparison; neither unsafe indexing, threaded dispatch, fusion nor shared
call windows is promoted by this profile. Budgets and checked arithmetic
remain required work, including when comparing against Lua.

**Compile breakdown.** `path_profile stages NAME 20001`, 1,000 warm-up
runs, retained raw individual intervals. Each fresh artifact executes and
its answer is checked outside the compiler timers. Times below are p50 /
p99, in µs; tiny stages are quantized by the ~42 ns clock. No overhead is
subtracted. Disposal is measured after execution and is excluded from the
original T011 compile interval.

| Workload | Lex | Parse | Check | Lower + artifact construction | Artifact disposal |
|---|---|---|---|---|---|
| module (1,236 B) | 1.917 / 2.500 | 1.125 / 1.458 | 0.916 / 1.167 | 2.416 / 3.125 | 0.250 / 0.375 |
| calls | 0.292 / 0.417 | 0.208 / 0.292 | 0.167 / 0.250 | 0.583 / 0.791 | 0.166 / 0.209 |
| arithmetic | 0.458 / 0.750 | 0.292 / 0.542 | 0.250 / 0.500 | 0.750 / 1.500 | 0.125 / 0.292 |
| branching | 0.875 / 1.125 | 0.583 / 0.792 | 0.417 / 0.583 | 1.375 / 1.875 | 0.167 / 0.250 |
| host_calls | 0.250 / 0.333 | 0.167 / 0.209 | 0.125 / 0.167 | 0.375 / 0.500 | 0.083 / 0.125 |

For the module, the frontend is about 4 µs and lowering about 2.4 µs,
roughly 38% of the sum of stage medians. This comparison uses the same
fixture; subtracting the earlier synthetic 40-function frontend's
ns/byte from the module's total was only a lead, not a phase measurement.
Reset and diagnostics lifecycle are outside the stage timers, so use T011
for the full compile cost; sums of medians/p99 are not total percentiles.

The `compile module` sample has 1,540/4,114 samples under lowering (37.4%).
The isolated lower/dispose sample has 1,452/3,932 under `Assembler::finish`
(36.9%), 965 in `encoding::encode` (24.5%), and 1,173 with allocator frames
(29.8%, including allocation, reallocation and freeing). These are
**overlapping inclusive shares**, not additive buckets. `finish` does branch
layout, output code/spans/start bits/literal metadata construction and owned
slice finalization; emission also uses the general operand encoder for
statically selected scalar operations. Those are the first compile-side
candidates to measure under T037. The profile does not justify changing
branch bounds, dropping diagnostics metadata or adding an arena wholesale.

**Cross-check and limits.** Reran `cargo bench -p eitri --bench
source_to_result -- 20001`, then `crates/eitri/benches/lua/run.sh 20001`,
without a profiler. Reused p50 compile/execute: module 6.542/0.250 µs;
arithmetic 1.750/18.9; branching 3.333/48.0; calls 1.292/28.3;
host_calls 0.917/10.5. Lua execution is 15.0 µs arithmetic, 31.3 branching,
14.3 calls and 10.6 host calls. The largest relative gap remains calls;
the small module still misses the few-microsecond target (6.792 µs total).
This is a profiling baseline, not a before/after speedup. The original
allocation, scratch, code and metadata measurements are unchanged by this
instrumentation. Sampling shares are approximate, target-specific and
subject to profiler/allocator effects; no hardware counters were collected.

Raw stage CSVs, `sample-{calls,arithmetic,branching,compile,lower}.txt`,
Instruments `.trace`/XML exports, PC histograms, disassembly, tool output,
source fingerprints, correctness output and T011/Lua CSVs are retained in
`target/bench-results/`. The compact evidence archive is
`target/bench-results/path-profile-2026-11-07.tar.gz`.
`cargo test --workspace`, `cargo fmt --all -- --check`, and
`cargo clippy -p eitri --example path_profile -- -D warnings` pass.
E4/E5 and T037 remain experiments to run, not completed optimization tasks.

### E15: per-run allocation vs reused scratch (lexer) — 2026-11-07

**Question.** Should the context own a memory context (a scratch arena or
pooled buffers) that compilation stages borrow, instead of each stage
allocating its own tables and freeing them when it finishes?

**Method.** `cargo bench -p eitri-syntax --features experiment-scratch
--bench lex_alloc`. Apple M5 (10 cores), macOS 27.0.1, Rust 1.97.1, LLVM
22.1.6, release profile, system allocator. Two strategies on the same
lexer, interleaved batch by batch: `lex` (fresh token arrays and stacks per
run, the shipped path) and `lex_reusing` (warm buffers kept between runs).
Reuse removes every allocation after warm-up, so its saving is the upper
bound for any arena. Each run resets the interner and checks the token
count. 201 batches of about 1 ms; single runs timed individually for p50 and
p99 (timer overhead below the 1 ns reporting resolution). Allocation counts
come from a counting allocator in the benchmark binary. Raw single-run
samples: `target/bench-results/lex_alloc.csv`.

| Workload | Ephemeral (batch p50) | Reused (batch p50) | Saving | Run p99, ephemeral / reused | Allocations per run |
|---|---|---|---|---|---|
| One line (`let answer = 42`) | 125 ns | 48 ns | 77 ns (62%) | 166 / 83 ns | 5 |
| `util.eit`, 39 lines, 985 B | 2,153 ns | 2,066 ns | 87 ns (4.0%) | 2,792 / 2,666 ns | 6 |
| `plugins.eit`, 130 lines, 4.3 KB | 7,053 ns | 6,931 ns | 122 ns (1.7%) | 11,666 / 11,625 ns | 6 |
| Whole reference config, 12 files, 26 KB | 49.4 µs | 48.3 µs | 1.1 µs (2.3%) | 63.8 / 61.9 µs | 66 |
| Reference config ×16, 415 KB | 772 µs | 768 µs | 4.5 µs (0.6%) | 872 / 854 µs | 6 |

**Findings.**

- The lexer makes a fixed number of allocations per file (three token
  columns plus small stacks), independent of file size, at about 15–20 ns
  each including the free. Reuse saves 2–4% on real modules and less on
  large ones. Only the one-line script shows a large share, because lexing
  it takes about 45 ns.
- Allocation count is bounded by the number of tables, not the number of
  nodes, because syntax is stored in index-addressed columns and pooled
  lists. Arenas earn their keep in compilers that allocate each node
  separately (R31); with this layout that per-node cost does not exist.
- Unexpected and larger: lexing itself costs about 2 ns per source byte
  (2.2 µs for a 1 KB module). That alone threatens the few-microsecond
  small-module goal and is the first thing to profile under E2.

**Decision.** The data alone supported keeping stages ephemeral (D43). The
project owner chose a context-held `Scratch` struct instead (D44), for one
home for reusable memory as more stages and the VM arrive; the measured
effect of that choice is the small saving above, not a regression. No bump
arena: per-table reuse already reaches the ceiling E15 measures. T011 must
report the retained scratch high-water, the cost of this choice. The
benchmark compares a fresh `LexScratch` per run with a kept one:
`cargo bench -p eitri-syntax --bench lex_alloc`.

### E2 (first pass): lexer throughput — 2026-11-07

**Question.** Where does the lexer spend its time, and which low-level
changes reduce it without changing behavior?

**Method.** Same machine and toolchain as E15. Throughput from
`cargo run --release -p eitri-syntax --example lex_profile` (the reference
config, 12 files, 25,921 bytes, 5,068 tokens per run; median of three
3-second runs). Profiles from Instruments' Time Profiler (`xctrace`), with
leaf addresses mapped to source lines through a dSYM. Every step had to pass
a differential test against a frozen copy of the original lexer
(`lexer/reference.rs`): identical kinds, spans, spellings, delimiter
partners, and diagnostics on the corpus, all fixtures, and 20,000 seeded
mutations and fragment strings.

**Profile before.** Main loop and dispatch 47%, interning 22%, token push
18% (three columns, each checking id overflow and capacity), string
scanning 11%.

| Step | Change | ns/byte | Result |
|---|---|---|---|
| 0 | Baseline | 1.88 | |
| 1 | Token push: one capacity check and one id for all three columns | 1.64 | kept (−12.5%) |
| 2 | Interner hit: compare bytes with overlapping word loads instead of `str` slicing and a `memcmp` call | 1.56 | kept (−5%) |
| 3 | SWAR scanning (8 bytes per step): identifier runs, indentation runs, line comments, string bodies | 1.37 | kept (−12%) |
| 4 | Step over one space after a token without a branch, bypassing the dispatch | 1.35 | kept (−1.7%) |
| 5 | Token push: unchecked writes after the shared capacity check (`unsafe`, below) | 1.29 | kept (−4.2%) |
| 6 | Short-key hash (one 128-bit multiply) instead of Fx | 1.29 | **rejected**: no change; hashing was not the cost |
| 7a | Dispatch through a 256-entry byte-class table | 1.65 | **rejected**: +27%, one more dependent load per token |
| 7b | Guard-free `match` on the byte (one jump table) | 1.50 | **rejected**: +16%, inlining the push path into ten arms bloated the loop |
| 8 | Plain-string fast path | 1.31 | **rejected**: no change on this corpus |
| 9 | SWAR for raw strings and block comments | 1.33 | kept: unchanged on the corpus (it has neither); 0.47 → 0.11 ns/byte on comment- and raw-string-heavy input |

**Result.** 1.88 → about 1.30 ns/byte on the reference config (−31%), 6.7 ns
per token. By workload (`lex_alloc`, kept scratch, batch p50): one line
33 ns (was 48), `util.eit` 1.42 µs (2.07), `plugins.eit` 4.68 µs (6.93),
whole config 33.9 µs (48.3), 415 KB 545 µs (768), 296 KB of comments and
raw strings 30.6 µs. Zero allocations per warm run throughout.

**`unsafe` (step 5).** `Tokens::push` writes each column's next slot
through a raw pointer after one shared check against the smallest column
capacity, then sets the lengths. The safe form repeats the capacity check
and length update per column because the compiler cannot prove the
capacities equal; removing them measured −4.2%. The invariant (equal
lengths, cached capacity at most every column's) is stated in a `SAFETY`
comment. Validation: the lexer tests pass under Miri (`cargo +nightly miri
test -p eitri-syntax -- --skip differential --skip adversarial`, 23 tests);
overstating the cached capacity by one is reported by Miri as an
out-of-bounds access, so the check would catch a broken invariant.

**Remaining.** Cost is now spread over per-token work: dispatch, interning
of each identifier (insertion dominates, since every compilation starts
from a reset interner), and the push. The next candidates need a
different structure rather than tuning, such as recognizing keywords
before interning or caching the newline rule's brace state, and each
should be measured against the parser's needs once T008 exists.

### E2 (second pass): parser and checker throughput — 2026-11-07

**Method.** Same machine and toolchain as E15.
`cargo run --release -p eitri-check --example frontend_profile` times lexing,
parsing, and checking separately on a synthetic 17,964-byte program in the
first slice (40 functions with loops, branches, recursion, and host calls);
three 3-second runs. Profiles from `xctrace` as in the first pass. Each step
had to pass differential tests against frozen copies of the T008 parser and
checker (`parser/reference.rs`, `check/reference.rs`): the same printed tree
and diagnostics on the corpus, 25 operator edge cases, and 10,000 seeded
mutations; the same diagnostics, types, resolutions, bindings, and literal
values on over 3,000 checked programs.

**Profile before.** Lexing 42%, parsing 35%, checking 23% (3.87 ns/byte,
69.5 µs per run). The parser descended through ten precedence functions per
operand, each re-reading the current token through a bounds-checked index;
the checker built its side tables one push per node and kept literal values
in a hash map.

| Step | Change | Stage ns/byte | Result |
|---|---|---|---|
| P1 | Precedence climbing (one loop per operator level, binding levels from a table) and the current token kind cached in the parser | parse 1.36 → 1.04 | kept (−23%) |
| C1 | Side tables sized with one `resize`; literal values in a vector referenced from the resolution slot instead of a hash map; decimal fast path for literal values | check 0.89 → 0.76 | kept (−15%) |
| C2 | Compare types before fetching a span; reporting moved out of line | check 0.76 → 0.69 | kept (−8%) |
| P2 | Load the operator span only for prefix operators | parse 1.04 → 1.03–1.06 | neutral; kept as it only removes work |

**Result.** 3.87 → about 3.35 ns/byte for the whole frontend (−13%),
69.5 → 59–61 µs per run. Lexing is now the largest share on this
identifier-heavy workload (about 1.6 ns/byte, 48%), mostly interning each
identifier occurrence.

**Found along the way.** The T008 parser handled `??` (outside the slice) by
right recursion, so `a ?? a ?? …` long enough would overflow the stack. The
precedence-climbing parser consumes it iteratively; a test parses a
100,000-long chain. The frozen reference keeps the recursion, since the
differential inputs never reach that depth.

**Follow-up (T009/T010).** `else if` chains recursed once per link in the
parser and checker; 100,000 links overflowed the stack (reproduced through
the CLI). Both now walk the chain in a loop, unwinding pending links from
scratch stacks so ids, types, and diagnostics come out as recursion gave
them (the differential tests still pass; tests check 100,000-link chains).
Inlined into its callers, the checker's loop cost about 3% of checking
(0.727 → 0.750 ns/byte, three runs each); with `if_expr` kept out of line
it measured 0.727–0.745 (median 0.731), within run-to-run noise. Parsing
was unchanged (1.046–1.056). One deliberate behavior change was made to
both the checker and its reference: an `if` with an expected type no
longer reports a mismatch of its own, because its branches already did
(`let x: i64 = if c { true } else { 1 }` gave the same error twice).

### E5/E4 first pass: the instruction head and the call copy — 2026-11-07

**Question.** The path profiles put 56–84% of VM samples in the shared
instruction head. Which of its costs can safe Rust remove, and what does
each removal buy?

**What the head did.** From the release disassembly of the loop: reload the
current function's code pointer and length, frame size, and the slot
vector's pointer and length from memory; bounds-check the code index;
validate the opcode byte through `Op::ALL`; re-slice the frame (an
overflow-checked add and a range check); decode all three operands with the
`WIDE` high byte merged in (about nine instructions, even for jumps, which
use none); then load from the jump table and branch. Roughly 35 instructions
before any work.

**Method.** `path_profile vm NAME 1` (compiled modules executed repeatedly,
every answer checked) on the T011 fixtures. Same machine, toolchain, and
release profile as T011. Each variant was built once into its own binary,
and the variants were run round-robin, alternating order each round, 7–9
rounds; medians of ns/run (aggregate throughput, not latency tails). An
earlier build-then-measure sequence showed ±5% swings from code layout
alone, which round-robin removes. Every variant passed the VM, lowering,
and fixture suites; a new test runs the oracle comparison through
`WIDE`-prefixed instructions (slots 300–302).

| Variant | Change | arith | branch | calls | host | module |
|---|---|---|---|---|---|---|
| base | T011 interpreter | 21,084 | 48,178 | 29,582 | 11,693 | 186 |
| V1 | Dispatch on the raw opcode byte instead of `Op::from_byte` | ±2–3% (sequential runs) | | | | |
| V2 | Code and frame held as local slices, re-derived only by `CALL`/`RET` | 17,915 | 44,159 | 27,616 | 9,964 | 186 |
| V3 | Operands decoded inside the handlers that use them | 16,062 | 39,157 | 26,722 | 8,262 | 170 |
| V4 | Call/return slot copies as an inline loop, not `memmove` | 15,361 | 37,763 | 23,082 | 8,614 | 153 |
| V4b | Inline single-slot copy, `copy_within` otherwise | 14,677 | 36,065 | 20,126 | 8,843 | 158 |
| V5 | Handlers expanded twice: `hi` constant 0 on the common path, the prefix word after `WIDE` | 12,808 | 34,013 | 19,307 | 7,286 | 144 |

(V5 is from a second round-robin against V4b, whose medians there were
14,629 / 35,730 / 19,891 / 8,402 / 161.)

**Results.** V1 was neutral: LLVM already folded the validation table into
the jump-table range check, so it was reverted. V2, V3, and V5 each paid
on every loop-bound workload. V4 cut the call workload 14% but slowed
`host_calls`, which makes no script calls, by about 5% in interleaved
runs: a code-generation side effect on the shared loop, not the copy
itself. V4b kept most of the call gain; `host_calls` stayed about 7% above
V3 until V5 recovered it. Kept: V2, V3, V4b, V5. All safe Rust; the
interpreter still cannot cause undefined behavior on unverified code.

**End to end (T011 bench, reused runtime, execute p50).** arithmetic 18.8
→ 11.7 µs (Lua 15.0), branching 43.1 → 30.8 µs (Lua 31.1), calls 27.0 →
18.6 µs (Lua 14.3), host calls 10.5 → 7.3 µs (Lua 10.2). Arithmetic and
host calls now beat Lua 5.4, branching is level, and script calls trail by
1.3× (about 9.4 ns per call and return against Lua's 7). Compile times are
unchanged, so the small module is still 6.6 µs end to end.

**Not concluded.** No hardware counters were read, so the gains are not
attributed to fewer instructions versus fewer stalls. Per-slot bounds checks
remain in every handler; removing them needs `unsafe` justified by the
verifier, which is a separate decision, not a step in this pass. The call
path's remaining cost (site lookup, depth check, control-frame push and
pop, callee and caller lookups) has not been profiled since V4b.

### E5 second pass: tail-call threaded interpreter and immediates — 2026-11-07

**Question.** Can a qualitative change, rather than further shaving of the
`match` loop, put Eitri clearly ahead of Lua 5.4, starting with script
calls (the remaining 1.3× gap after the first pass)?

**What changed.** Toolchain moved to the pinned `nightly-2026-11-05`
(rustc 1.101.0, LLVM 23.1.5) for `become` and `extern
"rust-preserve-none"`. Two independent changes:

1. `eitri_vm::run_threaded` (experimental, selected by
   `Context::set_threaded`): one handler per opcode, each ending in a tail
   call through a 256-entry table (R15, R19). The preserve-none convention
   keeps code, PC, the instruction word, and the frame pointer and length
   in registers across handlers (R5, R19). Steps, each kept only if
   measured:
   - T1, the handlers: loops 0.75–0.88× the `match` loop, calls 1.04×.
   - T2, no calls on fast paths: faults record and tail-call a cold `raise`
     handler; out-of-frame slots (unverified code only) trap with
     `abort_immediate` instead of panicking. Handlers dropped their stack
     frames except the frame-pointer pair macOS requires: loops 0.69–0.76×.
   - T3, call path: growth of the slot or frame stack goes to a cold
     `grow` handler that re-executes the `CALL` before any budget debit;
     the fast path copies through the frame pointer and pushes within
     capacity. `call_script` went from about 200 instructions with spills
     to 172 with almost none: calls 1.09× → 0.81×.
   - T4, one-slot argument and result copies without loops: calls −4.5%.
   Slots stay bounds-checked against the frame length, so unverified code
   still cannot cause undefined behavior; the frame pointer is derived
   from the whole slot buffer, and Miri (Stacked Borrows) passes the
   recursion, growth, call, fault, host-window, and reuse tests on both
   interpreters (recursion depth reduced to 1,000 under Miri).
2. C2, immediate forms from the spec: `ADDI`/`SUBI` (checked and
   wrapping), `MULI.CHK.S`, and `CBI.EQ/NE/LT/GT` implemented in the
   verifier, assembler (`branch_imm`, long form through `imm_complement`:
   `x < k` negates to `x > k - 1`), and both interpreters. Lowering uses
   them when one operand is a literal in range, on either side of a
   comparison; the literal has no effects, so source order is unchanged.
   `fib` went from 10 to 7 instructions on its recursive path.

Every step ran the VM tests, the 113 fixtures (on both interpreters, which
must agree on results, faults with traces, and host effects), and new
oracle tests for widened and immediate operations, including the far form
of every `CBI`.

**Method.** `path_profile vm NAME 1`, one binary with both interpreters
selected by `EITRI_THREADED`, run round-robin, 7 rounds, medians; then
the T011 bench and Lua harness in the same session.

| VM only, ns/run | `match` + C2 | threaded + C2 + T4 |
|---|---|---|
| arithmetic | 10,996 | 8,494 |
| branching | 26,942 | 17,421 |
| calls | 15,869 | 12,168 |
| host calls | 6,859 | 5,187 |
| module | 137 | 121 |

**End to end (T011, reused runtime, p50 of compile + first execution,
threaded).** module 6.5 µs (Lua 9.5), arithmetic 10.0 µs (Lua 17.2),
branching 20.4 µs (Lua 36.9), calls 13.3 µs (Lua 16.1), host calls 5.9 µs
(Lua 11.3): 1.2–1.9× faster than Lua 5.4 on every workload; execution
alone of `fib(15)` is 12.1 µs against Lua's 14.5 (about 6.1 ns per call
and return).

**Promoted** to the default on 2026-11-07 by the project owner, with the
`match` loop kept as the portable reference (`Context::set_threaded(false)`).
**Not concluded.** AArch64 (Apple M5) only; the promotion gates ask for
x86-64 as well, which is still owed. No hardware counters. The aborting
out-of-frame check changes unverified code's failure from a panic to a
trap, which needs a decision before promotion. Compile time is now the
larger part of the small module's latency (6.3 of 6.5 µs).

### Second machine: Apple M2 (MacBook Air) — 2026-11-07

**Method.** Commit `f4573b1` on an Apple M2 MacBook Air, macOS 27.2, the
same pinned `nightly-2026-11-05`; `cargo bench -p eitri --bench
source_to_result` with the threaded interpreter, again with
`EITRI_THREADED=0`, then `crates/eitri/benches/lua/run.sh` (Lua 5.4.9,
static, Apple clang 21 `-O2`), one after another, 20,001 samples per
cell. A first Lua run measured Lua 5.5.0 by mistake: that machine's
`opt/lua@5.4` was a stale Homebrew alias for `lua` 5.5. After installing
`lua@5.4` it was rerun; the harness now refuses to build against headers
other than 5.4.

**Results (reused runtime, p50 of compile + first execution).**

| Workload | Eitri threaded | Eitri `match` | Lua 5.4.9 | Lua ÷ threaded |
|---|---|---|---|---|
| module | 8.2 µs | 8.3 µs | 12.2 µs | 1.49× |
| arithmetic | 15.0 µs | 30.1 µs | 26.2 µs | 1.75× |
| branching | 36.7 µs | 56.9 µs | 52.5 µs | 1.43× |
| calls | 21.5 µs | 30.6 µs | 24.1 µs | 1.12× |
| host calls | 8.9 µs | 17.1 µs | 21.0 µs | 2.36× |

**Reading.** The threaded interpreter's advantage over the `match` loop
is larger on the M2 than on the M5: execution 2.2× faster on arithmetic
and 2.1× on host calls (M5: about 1.3×), so the `match` loop is slower
than Lua on the M2 for every loop workload while the threaded one is
faster on all of them. Script calls are the narrowest margin (execution
19.9 µs against Lua's 21.9). Compile time scales with the slower core
(module 8.0 µs; M5 5.7 µs) and stays 1.3–1.5× ahead of Lua's. Still
AArch64 only.

### Compile path, first pass: lowering — 2026-11-07

**Question.** Lowering and artifact construction were the largest compile
stage for the small module (2.2–2.3 µs of about 6.3 µs). What does it
spend that time on?

**Method.** `path_profile stages module 10001`, six alternating rounds per
pair of binaries, medians of per-run stage medians; `sample` profiles of
`path_profile lower module` with symbols. Same machine, pinned nightly.

**Profile before.** Self time: `encoding::encode` 28% (it re-derived every
field's position and range from the specification per instruction),
`Assembler::finish` 14%, the lowering walk 13%, allocator about 12%.
`finish` allocated three layout arrays per function and moved the
constant and call-site vectors out, so the next function started from
zero capacity.

| Step | Change | Lowering ns | Result |
|---|---|---|---|
| L1 | Layout arrays kept in the assembler; constants and sites copied out at exact size | 2,333 → 2,084 | kept (−11%; disposal +40 ns) |
| L2 | `encode` packs narrow instructions from a compile-time table built from `OPS`; any other case, including every error, takes the general encoder | 2,083 → 1,666 | kept (−20%) |
| L3 | Instruction words copied directly instead of `extend_from_slice` (a `memmove` call per instruction) | 1,666 → 1,583 | kept (−5%) |

A differential test checks the fast encoder against the general one on
every opcode with operands around every range boundary (about 326,000
encodings): same words, same errors.

**Result.** Lowering −32%. T011 module, reused runtime: compile 6.3 →
5.7 µs p50, source to result 5.8 µs (Lua 5.4: 9.5 µs); allocations per
compile 55 → 37. Lexing (about 1.9 µs, mostly interning identifiers) is
now the largest stage, then lowering (1.6 µs), parsing (1.2 µs), and
checking (0.9 µs). A fresh context still compiles about 2.5 µs slower than
a warm one while its scratch grows.

### Host reentry: the call context — 2026-11-07

**Question.** What does giving every host call a reentrant call context
(T010) cost on the host-call path, and how much native stack does a nested
run take?

**Change.** `Host::call(&mut self, cx: &mut Call, index)` replaces
`call(index, args, results)`. The context lends the shared run state
(slot and frame stacks, budget, limits), so arguments and results stay in
place in the caller's frame, reached by index; after the host returns, the
handler re-derives its frame pointer, since a nested run may have grown the
slot stack. The threaded interpreter embeds that shared state in its
handler state, so a host call builds only the context (caller frame,
windows, top of stack).

**Method.** Apple M5, macOS 27.0.1, pinned `nightly-2026-11-05` (rustc
1.101.0), release profile. `source_to_result` binaries for commit `7ec14a0`
and for the change, run alternately three times each (threaded, then
`EITRI_THREADED=0`), 20,001 samples per cell; reused-runtime execution p50.
Stack per nested run: the address of a local in a host function that
reenters 20 levels deep, averaged over 10 levels.

**Results.** Execution p50, threaded (`7ec14a0` → change): `host_calls`
5.0 → 5.5 µs (1,000 host calls: about 0.5 ns each), `calls` 12.3–12.5 →
11.7–12.2 µs, `arithmetic` 8.4 → 8.4 µs, `branching` and `module` within
run-to-run noise. `match` loop: `host_calls` 6.1–6.3 → 6.6 µs, `calls`
15.8–16.0 → 16.5–17.2 µs, others within noise. Two intermediate steps were
measured and not kept: without `#[inline]` on the context's accessors,
each `cx.arg`/`cx.set_result` was a cross-crate call (threaded
`host_calls` 7.1 µs); with `#[inline]` but a shared-state copy built per
host call, 5.9 µs. Stack per nested run, excluding the host's own frame:
688 B (`match`) and 896 B (threaded) in release, 14.9 KB and 3.5 KB in
debug. 256 nested runs overflowed a 2 MiB test thread in a debug build, so
the nesting limit is 64.

**Reading.** The remaining cost is building the context and reading the
host's result through memory; reentry itself is off the common path.
Kept: reentry is required (T010), and 0.5 ns per host call is small against
the sub-microsecond callback target. Revisit with the T024 embedding API.

### Hot tier: re-optimizing bytecode from recorded `.eits` — 2026-11-08

**Question.** Can long-running code be re-optimized at the `.eits` level
without making the first run of fresh source pay for an IR?

**Design.** The assembler records the logical instructions it is given
(`ir::Body`, `.eits` in memory) only when it is the recording
instantiation, `Assembler<true>`; lowering is generic over the same flag,
so first-run lowering contains no recording code. `Compiled` counts its
executions; past a threshold (`Context::should_optimize`, default 100) the
host calls `Context::optimize(&mut compiled, source)`, which lowers the
source again with recording, rejects it unless it reproduces the compiled
code word for word, runs the passes in `eitri_bytecode::opt`, re-assembles,
verifies every function, and replaces the code (an invalid result keeps the
original; debug builds fail). `optimize_kept` does the same from kept IR
(assembled `.eits`). Passes: argument-copy coalescing (a call fed only by
`MOV`s of consecutive variables uses their slots) and loop-invariant
literals (a literal used only by the next instruction, in a loop entered
only at its top, loads once before the loop into a fresh slot). Analysis is
block liveness over slots, bounded by `ANALYSIS_LIMIT` and `SCAN_LIMIT`.

**Method.** Apple M5, macOS 27.0.1, pinned `nightly-2026-11-05`, release
profile. Compile cost of recording support: `path_profile compile|lower`
(batched over 3 s, aggregate ns/run) for `bf5b64c` and the change,
alternated three times. Hot tier: the `source_to_result` hot-tier table,
2,001 `optimize` samples (compiled fresh each time, untimed) and 20,001
steady-state executions of one compiled program before and after, every
result checked.

**Results.** Recording with a runtime flag (off) cost `module` compile
+100–160 ns (2–3%) and lowering +80 ns; compiling the check out entirely
left most of it, and even the baseline assembler with stub methods was +45
ns in lowering, so it was layout and structure rather than the check. With
the compile-time split, full compiles are within noise: `module`
5,722–5,761 vs 5,722–5,801 ns, `host_calls` 735–736 vs 729–733 ns. Hot tier
(execute p50, tier 0 → optimized; optimize p50; break-even runs):
`arithmetic` 8,458 → 7,209 ns, 4.5 µs, 4; `branching` 17.3 → 16.0 µs,
9.0 µs, 7; `host_calls` 5,583 → 4,792 ns, 2.5 µs, 4; `calls` 12.0 → 11.9
µs, 3.3 µs, 78; `module` 125 → 125 ns, 16.0 µs, never. `optimize` includes
compiling the source again and verifying.

**Long-running processes (2026-11-08).** Each workload compiled once and
executed 10,000 times in one context (total includes compiling and, with
tier-up at the default threshold of 100 runs, optimizing), medians of five
processes; Lua 5.4.9 loads once and calls the chunk 10,000 times. Totals,
tier 0 / tier-up / Lua: `module` 1.45 / 1.57 / 1.97 ms, `arithmetic` 84.3 /
72.5 / 149.5 ms, `branching` 178.8 / 159.5 / 348.4 ms, `calls` 122.3 /
122.9 / 148.1 ms, `host_calls` 55.3 / 48.4 / 112.1 ms. Tier-up finishes
12–16% sooner where there are loops; `module` (straight-line, no change
found) finished 8% later, more than its 16 µs optimization alone accounts
for, unexplained so far.

**Reading.** Re-optimizing is worth it for loops after a handful of runs;
straight-line setup code gains nothing and should not be re-optimized (the
run count alone cannot tell; a report of zero changes can). Kept: the
tiering, both passes, and the split. Next passes, from the golden
listings: copies into a single-variable call argument when the variable is
not consecutive, and repeated loads of the same literal outside loops.

### Reentry handle identity: address and name hash, abandoned — 2026-11-08

**Question.** How can a host keep a `ScriptFn` (a script function it
reenters) across host calls without ever reaching another program's
function?

**Tried.** A `ScriptFn` held only a function index, so a cached handle
used while another program ran called that program's function at the same
index (an audit reproduced it returning another function's `99`). The
first fix stored the program's function-table address with the index,
plus a hash of the function name for tables reused after their program is
dropped. A second audit reproduced the stale case in 99 of 100 trials: the
allocator reuses the address, and a new program with another `callback` at
the same position matches both checks.

**Reading.** Without process-global state (excluded by design) no runtime
identity is guaranteed unique across a program's whole lifetime and its
successors': counters restart per context, contexts can reuse addresses,
and names repeat. Kept instead: `ScriptFn<'call>`, valid only during the
host call that looked it up, which the borrow checker enforces (a
`compile_fail` doctest) at no runtime cost. The price is a lookup by name
on each host call; retained handles are T024's, and must be measured
against that lookup before choosing a representation.

### T009/T010/T012: retained simple lowering and semantic gate — 2026-11-12

**Decision.** Retain `lower_simple` / `Context::compile_simple` as T009's
explicit comparison baseline. It copies binding operands, uses register
arithmetic, and materializes comparisons before testing them. It retains
cheap destination passing and temporary reuse, and shares the bounded,
iterative control-flow walks. Const specialization selects the lowering;
there is no stored context mode, runtime mode lookup, or new scratch buffer.
Both modes are O(source syntax) time/working memory. The simple mode may
need more frame slots and code and can hit implementation limits earlier.
No execution optimization is promoted by this work; the purpose is to keep
a measurable baseline for later experiments.

**Checks.** `cargo build --locked --workspace`, `cargo fmt --all --check`,
`cargo clippy --locked --workspace --all-targets -- -D warnings`, and
`cargo test --locked --workspace` pass. All 124 fixtures (0 pending) compare
both lowerings on both interpreters against their expectations and the
independent tree oracle; current hot-tier comparisons remain. The two new
fixtures cover destination reads and ordered effects before a comparison
faults. Seven lowering tests include iterative 100,000-link chains and
verification of both modes. Existing `.eits` goldens are unchanged.

Exact extended commands:

```sh
EITRI_DIFF_CASES=100000 cargo test --locked -p eitri --test differential -- --nocapture
EITRI_FUZZ_CASES=1000000 cargo test --locked -p eitri --test fuzz -- --nocapture
EITRI_FUZZ_CASES=1000000 cargo test --locked --release -p eitri --test fuzz -- --nocapture
cargo test --locked -p eitri --test fixtures --test limits --test lifecycle --test reentry --test eits -- --nocapture
```

100,000 generated programs agree, with 887,041 host effects compared:
43,433 success, 39,176 overflow, 7,759 division by zero, 5,050 budget,
3,190 call-depth, and 1,392 missing-callback faults. Each debug/release
mutation run passes one million inputs per target: 100,440 sources compile
and agree across modes/VMs/oracle; 28,171 assembly inputs assemble and
agree; 51,699 bytecode inputs verify and run on both VMs. These are seeded
mutations, not coverage-guided fuzzing. No new unsafe; Miri was not rerun.
Limits cover widened slots, far branches/constants, locals, and 64 nested
host runs. Lifecycle over 129 sources has exactly stable warm live bytes
across 20 further passes: default match/threaded 423,706/554,778 B, simple
match/threaded 423,686/554,758 B. Context destruction returns to baseline.
This closes T009, its dependent T010 gate, and T012; T047 remains open.

**T011 rerun.** Base commit `5c37c1a4a7091d9656c0bd8fb8b3ea721645ff5f`
with the existing documentation notes, then this uncommitted step. Apple M5,
MacBook Pro Mac17,2, 10 cores (4 Super/6 Efficiency), 24 GB, macOS
27.0.1 (26A434), AArch64. Pinned `nightly-2026-11-05`: rustc
1.101.0-nightly (`2d20b42b8`, 2026-11-04), LLVM 23.1.5. Release workspace
defaults, system allocator, no custom build flags. Lua 5.4.9 static library
from `/opt/homebrew/opt/lua@5.4`, Apple clang 21.0.0, `-O2 -std=c11`.
The harness's `sysctl` CPU field was empty in this environment; model/core
information above was read with `system_profiler SPHardwareDataType`.

```sh
cargo bench -p eitri --bench source_to_result
EITRI_THREADED=0 cargo bench -p eitri --bench source_to_result
crates/eitri/benches/lua/run.sh
```

The first command was run before and after the code change. Commands ran
sequentially, without concurrent tests. Five unchanged executable workload
pairs, checked results on every run, 20,001 individually timed samples per
cell after 1,000 warm-up runs. Fresh source is parsed/checked/lowered each
time; no source cache or JIT. Host declarations are already registered;
there are no source dependencies. Runtime creation/registration and file
I/O are outside the target intervals; host work is the minimal result/id
stub. Timer-pair p50/p99 0/42 ns, not subtracted. The harness also reports
fresh runtimes, allocations, metadata, frames, hot tier, and repeated
execution (10,000 executions, 5 processes). Frequency scaling/affinity were
not controlled; hardware counters were not collected. These are local
observations, not a controlled performance promotion.

Reused runtimes, microseconds **p50 / p99** from individual CSV rows
(total is compile plus first execution within each sample):

| Workload | Before total | After compile | After first execution | After total | After match total | Lua total |
|---|---|---|---|---|---|---|
| module | 5.667 / 7.375 | 5.666 / 7.167 | 0.167 / 0.209 | 5.833 / 7.375 | 5.792 / 7.333 | 8.709 / 12.083 |
| arithmetic | 9.833 / 12.417 | 1.458 / 1.834 | 8.375 / 10.584 | 9.833 / 12.458 | 12.209 / 16.042 | 17.125 / 21.542 |
| branching | 20.084 / 25.250 | 2.875 / 3.625 | 18.000 / 22.916 | 20.875 / 26.250 | 30.541 / 37.250 | 37.333 / 46.416 |
| calls | 13.292 / 16.666 | 1.083 / 1.375 | 12.167 / 15.334 | 13.250 / 16.708 | 18.125 / 22.917 | 16.000 / 20.084 |
| host_calls | 6.250 / 7.917 | 0.791 / 1.000 | 5.500 / 6.917 | 6.250 / 7.917 | 7.625 / 9.750 | 13.166 / 16.666 |

Default threaded allocations/bytes per reused run are unchanged:
module 37/2,765 B; arithmetic 15/814 B; branching 15/1,251 B;
calls 15/678 B; host_calls 7/448 B. Code bytes, metadata bytes, maximum
frame slots, and retained warm scratch are unchanged as well:

| Workload | Code B | Metadata B | Max frame slots | Warm scratch B |
|---|---|---|---|---|
| module | 344 | 1,571 | 19 | 25,249 |
| arithmetic | 88 | 389 | 6 | 11,604 |
| branching | 180 | 734 | 4 | 16,615 |
| calls | 52 | 289 | 6 | 10,526 |
| host_calls | 48 | 232 | 4 | 8,210 |

**Reading.** The default source-to-result median changes range from about
−0.3% to +3.9% in this single before/after pair; no statistical claim of a
speedup or absence of regression is made. The small module still takes
5.833 µs (7.375 µs p99), above the sub-microsecond-to-few-microseconds goal;
compilation remains its largest measured component. The existing compile
profiles remain the basis for future optimization, rather than weakening
semantics. Simple-mode latency was not measured here; this adds the path
for future equivalent-work comparisons, not an optimization claim.

Raw before/after/threaded/match/Lua CSVs, complete benchmark output, and
environment details are retained in `target/bench-results/t009-step1/`
(and `/tmp/codex-eitri-step1/`). Full correctness output and the review
report are `/tmp/codex-eitri-step1/*.log` and
`/tmp/codex-eitri-step1.md`. No goldens were rewritten.

### T047/T065: diagnostic snapshots, generated catalog, and M0 documentation — 2026-11-12

**Scope and decision.** Close T047's remaining coverage/catalog checks using
actual diagnostics and explicit documentation generation. Runtime/compiler/
renderer implementations, accepted syntax, code assignments, and messages
are unchanged. There is no additional success-path work, state, scratch,
unsafe code, or dependency. `E0204` remains reserved for T022; this does not
implement T023's declaration-validation mode or any future declaration type.

**Checks.** Seventeen reviewed snapshots cover every supported code's
structured host data and terminal rendering. Thirteen reuse the existing
source fixture corpus; one generated nesting-limit source and three invalid
`.eits` inputs exercise implementation-limit and assembler/verifier codes.
Missing/drifted/orphan snapshots fail; updates require the documented
`EITRI_BLESS=1` command. Existing renderer snapshots cover Unicode/ASCII,
colors, short/medium styles, notes, multiple files, multiline labels, and
source-boundary cases. Three combined source fixtures report independent
lexical/parser/checker mistakes once, in source order, with zero host effects.
Existing cascade fixtures still pass. Additional tests check stored reports
at 1/99/100/101/300 errors in each stage and valid compilation after reuse.
This is regression coverage, not a proof of cascade freedom for every input.

The catalog generator reads `Code::ALL` and updates only the marked section
of the first-slice chapter. Ordinary tests and `--check` are read-only;
`--write` is explicit. Tests cover stale content, marker validation,
idempotence, and preserved surrounding text. A local link checker was added
for T065's M0-exit check; it validates inline Markdown file/heading links,
including duplicate headings, excluding fenced examples and external URLs.
It found one stale README fixture fragment, now corrected. It is not a full
Markdown parser or a network URL check. Negative controls deliberately stale
the catalog and one snapshot and break links; each is rejected as expected,
then the modified tracked files are restored byte-for-byte.

Exact passing commands:

```sh
cargo build --locked --workspace
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo test --locked -p eitri-diag --test catalog --test render -- --nocapture
cargo test --locked -p eitri --test diagnostics --test fixtures --test lifecycle -- --nocapture
cargo run --locked -p eitri-diag --example diagnostic_catalog -- --check
python3 tools/check_doc_links.py
```

The catalog contains 17 codes; all 127 source fixtures pass with 0 pending.
Full command output, snapshot-generation and expected-failure control logs,
and the review summary are in `/tmp/codex-eitri-step2/` and
`/tmp/codex-eitri-step2.md`. Base commit: `3ac033c` (step 1).
T011 was not rerun: changes are confined to tests, documentation, and
explicit development tools, with no source-to-result-path implementation
change. No new performance or allocation result is claimed; step 1 remains
the latest measured baseline, including its missed small-module target.
No coverage-guided fuzzer, Miri, or sanitizer was run for this step.

### R31: Where compilers use arenas

V8 allocates parser AST nodes in a `Zone` and frees the tree in one step
after bytecode generation; a 2015 change moved inner-function nodes into a
shorter-lived zone freed after scope analysis
([v8pedia](https://v8pedia.void.app/frontend/parser),
[crrev](https://crrev.com/33ec0b79b8ea60dcccf1d445b0cbd2eed8e1a165)).
SpiderMonkey measured 10–35% parse-time gains from changing how parse
nodes were pooled ([bug 558971](https://bugzilla.mozilla.org/show_bug.cgi?id=558971)).
rustc places long-lived compiler data in `TypedArena`s and a
`DroplessArena` tied to the `'tcx` lifetime
([rustc_arena](https://doc.rust-lang.org/nightly/nightly-rustc/rustc_arena/index.html),
[rustc_middle arena](https://doc.rust-lang.org/nightly/nightly-rustc/src/rustc_middle/arena.rs.html)).
Zig instead reworked its AST into fixed-size nodes with a shared extra-data
array and stores tokens and nodes in `MultiArrayList` columns
([AST layout commit](https://git.jakstys.lt/motiejus/zig/src/commit/4dca99d3f6b732c415d270f0c97def144ed6d3b7),
[Hashimoto's notes](https://mitchellh.com/zig/parser)).
Limit: these are secondary sources and design notes, not controlled
measurements of arena versus heap for an index-based layout; E15 is that
measurement for Eitri's lexer.

## Not verified

These were looked for and not confirmed; they are not used as evidence:
the full text of Rohou et al. (R16, quoted secondhand); the VEE 2005 version
of the stack-versus-register study and its 47%/25% figures; the Context
Threading paper's full text (Berndl et al., CGO 2005; only the authors'
slides); Perais and Seznec's full paper (M6); the current Intel optimization
manual (Vol. 1); reports that Ice Lake's move elimination was disabled by a
microcode update; and any 2023–2026 study measuring superinstructions or
NaN-boxing on current CPUs (none found; R17 and R20 are the closest); the
primary text of the Armv8-A Programmer's Guide §6.2.5 and of the RISC-V
rationale (A3 and A4 are quoted secondhand); and Arm's own pages for A1 and
A2 (a mirror of Arm's XML was used).

## Decision ledger

All rows are proposals until their experiment passes. Parameter choices
are explicitly Eitri hypotheses even when the mechanism has precedent.

| Decision | Evidence | Counterargument / limit | Experiment |
|---|---|---|---|
| D1: typed register bytecode is the default candidate | R1, R2 | An AST or direct native backend may win first-result latency | E1 |
| D2: intern names while lexing; distinguish spelling from binding | R8; language scope rules | Lookup and hashing have a cost; lexing cannot resolve shadowing | E2 |
| D3: compact indexed syntax and reusable scratch arenas | R9 | Materialization can cost more than streaming; scratch reuse is D44, a bump arena is not adopted | E2, E15 |
| D4: collect signatures before body lowering | Existing forward-call, inference, and full-check semantics | A genuinely one-pass lexer-to-execution design violates those contracts | E2; semantic tests |
| D5: emit typed operations and propagate local constants | R3, R5 | Variant count and compilation overhead grow | E3 |
| D6: 32-bit words, short operands; a `WIDE` prefix for large slots, inverted short branch over `J_X` for far targets, and few extended forms (D36) | R1; Lua's implementation is precedent, not proof | 64-bit words or decoded records may be faster | E3 |
| D7: eight-byte frame slots on 64-bit targets | R1, R5; explicit full-width scalar semantics | Larger frames than packed slots; not a 32-bit-target design | E3 |
| D8: separate control frames; explicit slot-based calls | R5 execution model; Eitri suspension semantics | Call windows can increase moves and frame size | E4 |
| D9: portable dispatch first; test generated tail-call dispatch | R5, R7 | Host compiler may spill state or fail tail-call lowering | E5 |
| D10: selectively fuse common operations | R7, R13 | More handlers can hurt instruction-cache footprint | E5 |
| D11: dynamic caches only where static binding is impossible | R10, R5 | Bad guards can silently change semantics | E6 |
| D12: precise safepoints and roots; retain first non-moving collector | R6, R12; existing GC contract | Map production and zeroing add latency | E7 |
| D13: shared operation definitions generate execution tooling | R5, R13 | A generator has build and maintenance costs too | E5, E8 |
| D14: baseline native compiler competes with interpretation | R3, R4 | Executable-memory publication may dominate tiny functions | E8 |
| D15: choose tier from measured break-even, with host override | R4, R5; total-cost equation in execution design | Future invocation counts are uncertain | E8 |
| D16: prebuild handlers, stencils, and host descriptors | R4, R5 | Runtime binary and template footprint grow | E8, E9 |
| D17: optional whole-function AOT specialization | R6 | Offline dependency/toolchain cost; reload interaction | E9 |
| D18: SIMD lexer path remains an experiment | R11 | Tiny inputs and Eitri lexical state may erase the gain | E2 |
| D19: compare-and-branch never materializes a boolean; its handler compares registers so the host pair can fuse where the core supports it; fold the budget poll into the backward form | M1, R7, R17 | Fusion must not span a safepoint or a source line | E5 |
| D20: a size budget for hot handlers; slow paths out of line | M2, R19 | A budget can block a useful specialized opcode | E5 |
| D21: full-width slot stores; never read a slot wider than it was written | M5 | Wider stores for narrow scalars cost nothing measurable, or they do | E3 |
| D22: copy coalescing keeps executed slot moves rare | M4, R1 | Coalescing adds compile time | E3 |
| D23: code is immutable; cached sites use per-runtime side tables with forward-only states; in-place quickening of a private code copy is an experiment, only at `dyn`, host, and map-key sites | M6, R22, R23 | Side tables cost a guard dispatch on hits; private copies cost memory and a copy | E6 |
| D24: dispatch experiments pin the host compiler version and inspect the generated code | R16, R18 | More engineering per experiment | E5 |
| D25: no early native tier for setup scripts; the interpreter is the default | R20, R21, R24 | A hot callback may still deserve native code | E8 |
| D26 (experiment): check everything, then lower each function to bytecode lazily, at its first call | R21; V8's lazy compilation | Checked bodies, capture layouts, and lowering metadata must survive scratch reset in a retained arena; first-callback latency moves into a host call | E10 |
| D27: conservative frame scanning competes with precise stack maps for the first collector | R26 | Untagged integers can retain garbage | E7 |
| D28: a small ISA of families with fields; a field that would branch inside a handler is a dispatch field with generated handlers, a field that only feeds arithmetic is decoded in one handler | A1, A5, M2 | A generator to build and test; operand-field decoding costs a few instructions | E11 |
| D29: fixed field positions; the dispatch sequence predecodes opcode and A | A6, R19 | Extended forms move A; Rust's `match` may not hoist the decode | E5 |
| D30: wrapping arithmetic, equality, and bitwise operations are sign-agnostic; signedness only where results differ | A1 | None semantic; the disassembler must print the source type | Semantic tests |
| D31: integer slots hold canonical sign- or zero-extended values; narrow arithmetic is the 64-bit operation plus one `CVT`, except shifts, which carry their width because the amount check depends on it, and narrow left shifts also their signedness | A1, M5, D21; language shift rules | Narrow arithmetic costs two dispatches; shift widths and left-shift signedness cost 15 register and 9 immediate opcodes | E11; fuzzing against narrow reference operations |
| D32: compares fuse into `CB`, `CBI`, `FCB`, `CSET`, and `FCSET`; no flags slot; integer and float conditions are separate families with 3-bit fields and `f32`/`f64` variants; float conditions include unordered forms so every condition has an exact complement; operand swap and immediate adjustment | A2, A4 (secondary), M1, M5 | Immediate adjustment fails at the edge of the range and needs the register form | E5, E11 |
| D33: no predication; `SEL` only if measured | A3 (secondary), M7 | `SEL` wins only when both operands are already in slots; with constants, the branch form executes as few or fewer dispatches | E11 |
| D34: load/store discipline with four addressing modes; access width, stride, extension, and slot width are separate, with width a dispatch field for ordinary arrays and fields, not only `@extern` | A1, D21, M5 | A field read plus an operation is two dispatches where a memory-operand form is one; 28 load/store opcodes | E11 |
| D35: pair move, and a range back edge with an entry test and overflow-free exclusive and inclusive forms | A1, R14 | Five more handlers; other strides use ordinary instructions | E11; boundary tests at each type's maximum |
| D36: a hand-counted, later generated, opcode manifest that the build checks against 256 | Review of D28–D35 | The first subset uses 163 of 256 before maps, enums, closures, `dyn`, and tasks; reviews found three undercounted dimensions (shift signedness, float precision, unordered conditions), so the count must be generated, not hand-maintained | E11 |
| D37: lowering describes values as constant, slot, or pending operation, and materializes lazily | R28 | Effects must stay in order; the descriptor adds lowering state | E3 |
| D38: a single-pass slot allocator that reuses dead temporaries in stack order and keeps loop and parameter values below 256 | R14, R28 | Stack-order reuse can leave longer-lived values in high slots | E3 |
| D39: carry one integer and one float temporary in host registers, scalars only, for the hottest families | R19 | Variants multiply (3 per opcode with one register, 10 with two) and spend opcode space | E12 |
| D40: literal allocations are pre-sized from compile-time counts | R29 | Saves little if literals are small | E14 |
| D41: hot call information separate from root maps and debug data; identical root maps shared; total artifact bytes reported | R12, D12 | Split tables add an indirection on the cold path | E13 |
| D42: fusion candidates come from measured pairs and sequences in the corpus, with composition rules checked by the specification | R30, R16, R17 | Corpus-specific; a candidate can overfit the reference config | E5 |
| D43 (superseded by D44): compilation stages allocate their own tables per run; the context keeps only the interner | E15; R31 | Allocation is a fixed cost per table, so it matters most for one-line scripts (62% of lexing a one-liner, but 77 ns absolute) | E15 |
| D44: the context owns a `Scratch` struct with one field of reusable working memory per kind of scratch (first: `LexScratch`); stages are functions that borrow their field and clear it on entry. No bump arena | Owner's design choice: one home for reusable memory as stages and the VM arrive; E15 shows the gain is small but positive (2–4% of lexing real modules, 77 ns on a one-liner, zero allocations when warm) | Retains the high-water memory of the largest run; a stage that forgets to clear leaks state between runs (tested per stage) | E15; T011 reports scratch high-water |

## Experiments and promotion gates

Use the [existing corpus](performance.md), both fresh and loaded dependencies,
on AArch64 and x86-64 before claiming portable wins. Initial single-target
results must be labeled. Preserve budgets, faults, rooting, and host calls.
Do not benchmark a feature-rich candidate against a baseline with different
semantics. Publish source, build flags, machine details, and raw timings.

| Experiment | Controlled comparison | Report |
|---|---|---|
| E1 | Same checked subset: typed tree vs bytecode vs immediate baseline native | Frontend, lowering, publication, first result, retained bytes; N=1 and repeated calls |
| E2 | Intern-on-lex vs intern-on-resolve; token buffer vs streaming; scalar vs selective SIMD | ns/source byte by size, allocations, identifier hit rate, long-string and deeply nested-lambda cases |
| E3 | 32-bit vs 64-bit instructions; accumulator vs pure register forms; short/extended frequency; 8-byte vs packed slots; full-width vs narrow slot stores | Emission time, bytes, executed dispatches and slot moves, frame size, decode cost, store-forwarding stalls |
| E4 | Separate copied call frames vs liveness-safe shared call windows | End-to-end latency, argument/result moves, frame growth, nested calls and reentry |
| E5 | Rust match vs generated dispatch; plain vs fused handlers; alternative IP updates; budget poll folded into backward branches | First result, cycles/op, indirect-branch MPKI, µop-cache vs legacy-decoder delivery, handler bytes, spills; host compiler version pinned and disassembly inspected |
| E6 | No cache vs side-table caches vs in-place quickening of a private code copy | Miss/hit costs, megamorphic behavior, GC ownership, different maps of the same static type, memory and copy cost of private copies |
| E7 | Conservative frame scanning vs typed-slot lifetime maps vs precise liveness maps; split unions | Compiler time, root metadata bytes, falsely retained bytes, frame bytes, GC pauses |
| E8 | Small emitting backend vs copy-and-patch; interpreter vs native tier | First-use and reused-code-pool cost, publication/cache synchronization, break-even N, executable bytes, runtime binary size |
| E9 | Prechecked descriptors and optional script AOT | Startup including linking/verification, artifact size, target compatibility, reload restrictions |
| E10 | Eager lowering of every function vs lazy lowering at first call, with checking always complete | First result for the reference config, total time including later callback calls, retained bytes for unlowered functions, first-callback latency |
| E11 | Family ISA with generated handlers vs one opcode per operation, type, and operand kind, on the same subset; `SEL` vs branches; `LD` plus operation vs a memory-operand form; narrow arithmetic as operation plus `CVT` vs fused | Handler count and bytes, executed dispatches and words, first result, µop-cache delivery, frequency of narrow arithmetic and of immediate adjustment falling back to the register form |
| E12 | Slot-only handlers vs one cached integer temporary vs one integer and one float | Executed loads and stores, handler count and bytes, opcode space used, spills in the assembly, cycles on the feature workloads |
| E13 | One `CallSite` record vs hot/cold split with shared root maps | Total executable-artifact bytes, descriptor bytes per call site, call-handler loads, GC root-scan time |
| E14 | Growing literal construction vs pre-sized allocation | Allocations and reallocations during the reference config's setup, first result |
| E15 | Fresh per-run allocation vs warm buffer reuse (the ceiling for any arena or memory context), per compilation stage | Time per stage by workload size, allocations and bytes per run, absolute and relative saving |

A candidate is promoted only after semantic equivalence checks and a measured
improvement on its intended workload without an unexplained regression in
the first-result corpus. There is no universal percentage gate yet: report
confidence and absolute costs, then choose an explicit regression budget.
Use differential tests across tiers, forced GC at safepoints, extended-operand
boundary cases, host reentry, suspension, overflow, and cleanup faults.


## T013 scalar completion audit — 2026-11-12

T013 retains eight-byte slots (D7/D21): `i8` is sign extended, `u8` and
`char` are zero extended, and `f32` occupies the low 32 bits with a zero
upper half. The verifier checks literal canonicality and typed conversion
formats. Narrow arithmetic uses full-width operations and checked or wrapping
canonicalization (D31); float conditions use the existing ordered/unordered
families (D32). Source float conditions lower to `FCSET` plus a boolean
branch; fused `FCB` remains unsupported. Storage-only extern widths remain
outside the source scalar set. No new unsafe blocks or execution tier were
introduced. Numeric decisions are recorded in [Types](../language/types.md#explicit-numeric-conversions).

The audit corrected shift contextualization: `256 << count` must remain
`i64` when `count` is `u8`; the count cannot narrow the left operand. Tests
cover every integer count type, direct decimal `f32` parsing on both sides
of a halfway value, ties-to-even, signed underflow, and narrowing infinity.
Independent integer/IEEE-field models check exhaustive narrow arithmetic,
random full-width arithmetic, conversions, all float operations and conditions.
Source boundary matrices additionally run through both lowerers, both VMs,
the hot tier, and the tree oracle. The tree oracle shares the frontend;
explicit literal expectations cover rounding independently of that frontend.

**T011 rerun.** `cargo bench --locked -p eitri --bench source_to_result`,
Apple M5, AArch64 macOS 27.0.1; Rust nightly 1.101.0
(`2d20b42b8`, 2026-11-04), LLVM 23.1.5; workspace release profile, default
threaded VM, no extra build flags. 20,001 individual samples per cell,
1,000 warm-up runs, freshly compiled source each run; file I/O and context
creation are excluded from compile/execute totals. The harness checks each
result, also reports fresh-context and 10,000-run hot-tier cases. Timer pair
p50/p99: 0/42 ns. CPU model was read separately because the sandbox hid it
from the harness. Frequency scaling was not controlled; these are local
observations, not cross-platform or controlled regression claims.

| Reused context workload | Compile p50 / p99 | Execute p50 / p99 | Total p50 / p99 |
|---|---|---|---|
| module | 7.500 / 14.8 µs | 0.208 / 0.334 µs | 7.708 / 15.4 µs |
| arithmetic | 1.916 / 3.583 µs | 11.0 / 19.3 µs | 12.9 / 24.2 µs |
| branching | 3.833 / 8.000 µs | 23.0 / 46.3 µs | 26.8 / 53.2 µs |
| calls | 1.542 / 3.500 µs | 16.5 / 35.9 µs | 18.1 / 39.6 µs |
| host_calls | 1.000 / 1.834 µs | 7.208 / 13.5 µs | 8.209 / 15.5 µs |

The 1,236-byte module still misses the few-microsecond goal. Compilation
dominates its latency. Its reused run allocates 37 times / 2,765 bytes,
retains 25,249 bytes of context scratch, and emits 344 code bytes plus
1,571 metadata bytes with 19 maximum frame slots. Across the five workloads:
7–37 allocations, 448–2,765 allocated bytes, 8,210–25,249 retained bytes.
This is a completion baseline, not a before/after optimization comparison;
Lua was not rerun. Full report and raw individual samples are retained locally
at `target/bench-results/t013-2026-11-12/{report.txt,source_to_result.csv}`.


### T015-1 managed string slice

On 2026-11-13, `cargo bench --locked -p eitri --bench source_to_result --
20001` measured 20,001 individually timed samples per cell, release profile,
default threaded interpreter, macOS 27.0.1 AArch64, nightly Rust 1.101.0
(2d20b42b8) and LLVM 23.1.5. The sandbox did not expose the CPU model.
The reused 1,236-byte module measured compile p50/p99 6.625/11.5 µs,
execute 0.167/0.291 µs, and total 6.833/11.8 µs; it allocated 48 times /
4,668 bytes, with 344 code bytes and 2,978 metadata bytes. Runtime creation,
file I/O and host work are excluded from the reused-context operation. The
few-microsecond source-to-result goal remains missed.

An earlier run of the same command on this stage, before bypassing managed
metadata setup for scalar-only code, measured total p50/p99 7.417/9.333 µs
and 61 allocations / 5,021 bytes. The runs were not interleaved, so their
latency difference is not a controlled performance claim. Both raw sample
sets and complete reports are retained in `/tmp/orchestra/eitri-t015-1-{before,after}-samples.csv`
and `/tmp/orchestra/eitri-t015-1-bench{,-after}.txt`. Forced-GC behavior is
covered by correctness tests, not timed here; no Lua or managed-workload
comparison is claimed. Hash-based string lookup and marking have expected
linear collection work but quadratic collision worst cases.


### T015-2 managed control flow

On 2026-11-13, `cargo bench --locked -p eitri --bench source_to_result --
20001` measured 20,001 individual samples per cell with the same release
profile, threaded interpreter, OS and compiler as T015-1 (CPU model unavailable).
The reused scalar 1,236-byte module measured compile p50/p99 6.584/10.0 µs,
execute 0.167/0.250 µs, total 6.791/10.3 µs, 48 allocations / 4,668 B,
344 code bytes and 2,978 metadata bytes. Context creation, file I/O and host
work are excluded. The few-microsecond target remains missed. Raw samples and
report: `/tmp/orchestra/eitri-t015-2-samples.csv` and
`/tmp/orchestra/eitri-t015-2-bench.txt`. The run was not interleaved with the
T015-1 baseline and does not measure managed workloads; it supports no causal
latency claim. Managed analysis costs and its unbounded compiler-memory risk are
recorded in [VM garbage collection](vm.md#garbage-collection). No conservative
scanning experiment or Lua comparison was performed.


### T015-3 host strings

On 2026-11-13, `cargo bench --locked -p eitri --bench source_to_result --
20001` measured 20,001 individual samples per cell, release profile, threaded
interpreter, macOS 27.0.1 AArch64 and Rust 1.101.0-nightly with LLVM 23.1.5;
the CPU model was unavailable. The reused scalar module measured compile
p50/p99 7.375/12.4 µs, execution 0.208/0.333 µs, total 7.583/12.8 µs,
48 allocations / 4,668 B, 344 code bytes and 2,978 metadata bytes. The
host-call workload measured execution 9.792/12.5 µs (p50/p99), versus
6.542/10.5 µs in the non-interleaved T015-2 run; this difference is a signal
for profiling, not a controlled estimate of overhead. This benchmark does not
measure string calls. Context creation, I/O and host work are excluded from
the reused run. The few-microsecond target remains missed. Report and raw
samples: `/tmp/orchestra/eitri-t015-3-bench.txt` and
`/tmp/orchestra/eitri-t015-3-samples.csv`. No Lua comparison was run.
