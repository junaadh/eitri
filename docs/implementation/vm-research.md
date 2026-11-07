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
| D3: compact indexed syntax and reusable scratch arenas | R9 | Materialization can cost more than streaming | E2 |
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

A candidate is promoted only after semantic equivalence checks and a measured
improvement on its intended workload without an unexplained regression in
the first-result corpus. There is no universal percentage gate yet: report
confidence and absolute costs, then choose an explicit regression budget.
Use differential tests across tiers, forced GC at safepoints, extended-operand
boundary cases, host reentry, suspension, overflow, and cleanup faults.
