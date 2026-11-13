use std::cell::Cell;

use eitri_base::{IndexVec, Interner, SourceId, Span};
use eitri_bytecode::ir::Insn;
use eitri_bytecode::text::{self, HostDecl, PrintOptions};
use eitri_bytecode::{CallSite, CallTarget, Callees, FunctionId};
use eitri_check::{CheckScratch, HostFnId, Hosts, Input};
use eitri_diag::{Code, Diagnostic, Diagnostics, Label};
use eitri_lower::{LowerScratch, Program};
use eitri_syntax::{LexScratch, ModuleKind, ParseScratch};
use eitri_types::PortableSignature;
use eitri_vm::{Fault, Host, Limits, Location, VmScratch};

/// Diagnostics kept per run before the rest are only counted (T047).
const DIAGNOSTIC_LIMIT: usize = 100;

/// Executions after which code counts as hot, until measured otherwise.
pub const DEFAULT_TIER_THRESHOLD: u32 = 100;

/// Why [`Context::optimize`] did not run.
#[derive(Clone, Debug)]
pub enum OptimizeError {
    /// The destination registry differs at a host index used by this artifact.
    HostBindings(FaultReport),
    /// The source no longer compiles in this context.
    Compile(Vec<Diagnostic>),
    /// The source compiles to code other than the program being optimized:
    /// it is not the source that program was compiled from.
    SourceMismatch,
    /// [`Context::optimize_kept`] on code that kept no logical
    /// instructions.
    NoIr,
    /// The retained function failed verification before optimization.
    InvalidBytecode(eitri_bytecode::VerifyError),
}

/// Caller-owned execution context, borrowed exclusively during submission.
///
/// It holds state that outlives a run (the interner, registered host
/// modules, limits) and the scratch memory stages reuse between runs.
/// Stages themselves are plain functions: each borrows its scratch for one
/// run. No global state or locks.
pub struct Context {
    /// Spellings. The predefined keyword table and host module names are
    /// committed and survive each run's reset.
    interner: Interner,
    hosts: Hosts,
    limits: Limits,
    /// Execute with the tail-call threaded interpreter (the default) or the
    /// portable `match` loop (E5).
    threaded: bool,
    /// Executions after which [`Context::should_optimize`] reports code
    /// as hot.
    tier_threshold: u32,
    /// Keep each compiled function's logical instructions, for
    /// [`Context::disassemble`] and re-optimizing. Off for first runs.
    keep_ir: bool,
    scratch: Scratch,
}

/// Working memory reused across runs, one field per kind of scratch. Each
/// stage clears its own field when it starts, so nothing leaks between
/// runs, and warm runs do not reallocate.
#[derive(Default)]
struct Scratch {
    lex: LexScratch,
    parse: ParseScratch,
    check: CheckScratch,
    lower: LowerScratch,
    vm: VmScratch,
    /// For re-assembling optimized code.
    recording: eitri_bytecode::Assembler<true>,
}

/// Why a run did not succeed.
#[derive(Clone, Debug)]
pub enum RunError {
    /// Rejected before anything ran: no host function was called.
    Compile(Vec<Diagnostic>),
    /// Stopped by a fault, after the host calls made before it.
    Fault(FaultReport),
}

impl RunError {
    /// The error as diagnostics, for rendering.
    pub fn into_diagnostics(self) -> Vec<Diagnostic> {
        match self {
            RunError::Compile(diagnostics) => diagnostics,
            RunError::Fault(report) => vec![report.diagnostic()],
        }
    }
}

/// A pre-execution binding rejection, or a runtime fault and its trace
/// (innermost first). Binding rejections carry a code and an empty trace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaultReport {
    /// A typed pre-execution rejection; runtime faults have no diagnostic code.
    pub code: Option<Code>,
    pub message: String,
    pub trace: Vec<TraceFrame>,
}

/// One trace entry: a script function and its faulting operation or active
/// call, or a host function (no span).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceFrame {
    pub function: String,
    pub span: Option<Span>,
}

impl FaultReport {
    /// The fault as a diagnostic: the faulting operation is the primary
    /// label, each active call a secondary one, a host function a note.
    /// A run of identical frames (deep recursion) becomes one label.
    pub fn diagnostic(&self) -> Diagnostic {
        let mut diagnostic = match self.code {
            Some(code) => Diagnostic::error(code, self.message.as_str()),
            None => Diagnostic::fault(self.message.as_str()),
        };
        let mut innermost = true;
        for run in self.trace.chunk_by(|a, b| a == b) {
            let frame = &run[0];
            let message = match run.len() {
                1 => format!("in `{}`", frame.function),
                n => format!("in `{}` ({n} frames)", frame.function),
            };
            diagnostic = match frame.span {
                Some(span) if innermost => {
                    innermost = false;
                    diagnostic
                        .with_label(Label::primary(Context::SOURCE, span).with_message(message))
                }
                Some(span) => diagnostic
                    .with_label(Label::secondary(Context::SOURCE, span).with_message(message)),
                None => {
                    diagnostic.with_note(format!("raised by host function `{}`", frame.function))
                }
            };
        }
        diagnostic
    }
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

impl Context {
    /// The id diagnostics use for the submitted source. Register that source
    /// first in the [`eitri_diag::Files`] used for rendering.
    pub const SOURCE: SourceId = SourceId::from_u32(0);

    pub fn new() -> Self {
        Self {
            interner: Interner::new(),
            hosts: Hosts::new(),
            limits: Limits::default(),
            threaded: true,
            tier_threshold: DEFAULT_TIER_THRESHOLD,
            keep_ir: false,
            scratch: Scratch::default(),
        }
    }

    /// Steps per run: each call and each taken backward branch costs one.
    pub fn set_budget(&mut self, steps: u64) {
        self.limits.budget = steps;
    }

    /// Active script function frames per run (top-level code not counted).
    pub fn set_depth(&mut self, frames: u32) {
        self.limits.depth = frames;
    }

    /// Choose the interpreter: the tail-call threaded one (the default) or,
    /// with `false`, the portable `match` loop. Both produce identical
    /// outcomes; the choice exists to compare them (research ledger, E5).
    pub fn set_threaded(&mut self, threaded: bool) {
        self.threaded = threaded;
    }

    /// Executions after which [`Context::should_optimize`] reports a
    /// program as hot ([`DEFAULT_TIER_THRESHOLD`] by default).
    pub fn set_tier_threshold(&mut self, runs: u32) {
        self.tier_threshold = runs;
    }

    /// Whether `compiled` has run often enough to be worth
    /// [`Context::optimize`], and has not been optimized yet.
    pub fn should_optimize(&self, compiled: &Compiled) -> bool {
        !compiled.optimized && compiled.runs() >= self.tier_threshold
    }

    /// Re-optimize hot code: compile `source` again with its logical
    /// instructions kept, check it is the source `compiled` came from (the
    /// same code, word for word), run the optimizer's passes, and replace
    /// `compiled`'s code with the result, verified. Behavior is unchanged:
    /// the same effects, faults, traces, and budget debits. Returns what the
    /// passes changed.
    pub fn optimize(
        &mut self,
        compiled: &mut Compiled,
        source: &str,
    ) -> Result<eitri_bytecode::opt::Report, OptimizeError> {
        self.check_bindings(compiled)
            .map_err(OptimizeError::HostBindings)?;
        let keep = std::mem::replace(&mut self.keep_ir, true);
        let fresh = self.compile(source);
        self.keep_ir = keep;
        let fresh = fresh.map_err(OptimizeError::Compile)?;
        let (old, new) = (&compiled.program.functions, &fresh.program.functions);
        // Everything the code carries: words, constants, sites, signatures,
        // literal types, frame sizes, and spans. Code alone can coincide:
        // `return 1` and `return true` have the same words.
        let same =
            old.len() == new.len() && old.iter().zip(new.iter()).all(|(a, b)| a.equivalent(b));
        if !same {
            return Err(OptimizeError::SourceMismatch);
        }
        compiled.program.bodies = fresh.program.bodies;
        self.optimize_kept(compiled)
    }

    /// Re-optimize code that kept its logical instructions: assembled
    /// `.eits`, or code compiled with [`Context::set_keep_ir`]. As
    /// [`Context::optimize`], without compiling again.
    pub fn optimize_kept(
        &mut self,
        compiled: &mut Compiled,
    ) -> Result<eitri_bytecode::opt::Report, OptimizeError> {
        self.check_bindings(compiled)
            .map_err(OptimizeError::HostBindings)?;
        if compiled.program.bodies.is_none() {
            return Err(OptimizeError::NoIr);
        }
        let mut report = eitri_bytecode::opt::Report::default();
        let functions = &mut compiled.program.functions;
        let hosts = eitri_lower::host_signatures(&self.hosts);
        let signatures = Program::signatures_of(functions);
        let callees = Callees {
            functions: &signatures,
            hosts: &hosts,
        };
        // Validate the retained artifact before any rewrites. Unsupported
        // metadata is preserved verbatim instead of silently regenerated.
        for function in functions.iter() {
            eitri_bytecode::verify(function, &callees).map_err(OptimizeError::InvalidBytecode)?;
        }
        let bodies = compiled.program.bodies.take().expect("checked above");
        let mut kept = IndexVec::with_capacity(bodies.len());
        for (function, body) in functions.as_mut_slice().iter_mut().zip(bodies.iter()) {
            if !function.roots.can_regenerate_scalar() {
                report.skipped += 1;
                kept.push(body.clone());
                continue;
            }
            let optimized = eitri_bytecode::opt::optimize(body, function, &mut report);
            let built = eitri_bytecode::ir::assemble(
                &optimized,
                &function.name,
                function.signature.clone(),
                &mut self.scratch.recording,
            );
            match built {
                Ok((code, body)) if eitri_bytecode::verify(&code, &callees).is_ok() => {
                    *function = code;
                    kept.push(body);
                }
                _ => {
                    // Keep the original: an optimizer bug must not change
                    // what runs. Tests run in debug builds and catch it.
                    debug_assert!(
                        false,
                        "the optimizer produced invalid code for `{}`",
                        function.name
                    );
                    kept.push(body.clone());
                }
            }
        }
        compiled.program.bodies = Some(kept);
        compiled.optimized = true;
        Ok(report)
    }

    /// Keep the logical instructions (`.eits` in memory) of code compiled
    /// from now on, so it can be printed ([`Context::disassemble`]) or
    /// re-optimized. Off by default: recording costs a little on every
    /// compile, and a first run does not need it.
    pub fn set_keep_ir(&mut self, keep: bool) {
        self.keep_ir = keep;
    }

    /// Register a host module: a declaration file of bodiless `pub fn`s
    /// that scripts can import as `path`. Diagnostics refer to
    /// `declarations` as source 0.
    pub fn register_host_module(
        &mut self,
        path: &str,
        declarations: &str,
    ) -> Result<(), Vec<Diagnostic>> {
        self.hosts
            .register(path, declarations, &mut self.interner)
            .map(drop)
    }

    /// Whether a host module is registered as `path`.
    pub fn has_host_module(&self, path: &str) -> bool {
        self.hosts.find(path).is_some()
    }

    /// The index [`Host::call`] receives for function `name` of the host
    /// module registered as `path`.
    pub fn host_index(&self, path: &str, name: &str) -> Option<u32> {
        let module = self.hosts.find(path)?;
        self.hosts
            .module(module)
            .fns
            .iter()
            .copied()
            .find(|&f| self.interner.resolve(self.hosts.function(f).name) == name)
            .map(HostFnId::as_u32)
    }

    /// Compile and run UTF-8 source: [`Context::compile`], then
    /// [`Context::execute`].
    pub fn run(&mut self, source: &str, host: &mut dyn Host) -> Result<(), RunError> {
        let compiled = self.compile(source).map_err(RunError::Compile)?;
        self.execute(&compiled, host).map_err(RunError::Fault)
    }

    /// Compile UTF-8 source. Each stage runs only if the previous one
    /// reported no errors; the result exists only if the whole module
    /// checks, so rejected source never executes. Compiling invalidates
    /// nothing previously compiled.
    pub fn compile(&mut self, source: &str) -> Result<Compiled, Vec<Diagnostic>> {
        self.compile_with::<false>(source)
    }

    /// Compile with the retained simple lowering baseline (T009), for
    /// semantic and performance comparisons. Uses the same frontend and
    /// budgets as [`Self::compile`], with additional operand copies and
    /// register operations instead of immediate arithmetic/fused comparisons.
    /// This can need more frame slots; implementation limits still apply.
    pub fn compile_simple(&mut self, source: &str) -> Result<Compiled, Vec<Diagnostic>> {
        self.compile_with::<true>(source)
    }

    fn compile_with<const SIMPLE: bool>(
        &mut self,
        source: &str,
    ) -> Result<Compiled, Vec<Diagnostic>> {
        // No spelling from a previous compile is referenced any more:
        // compiled code holds names as strings, not symbols.
        self.interner.reset();
        let mut diagnostics = Diagnostics::new(DIAGNOSTIC_LIMIT);
        let tokens = eitri_syntax::lex(
            source,
            Self::SOURCE,
            &mut self.interner,
            &mut diagnostics,
            &mut self.scratch.lex,
        );
        if diagnostics.has_errors() {
            return Err(diagnostics.into_vec());
        }
        let ast = eitri_syntax::parse(
            source,
            Self::SOURCE,
            tokens,
            ModuleKind::Script,
            &mut diagnostics,
            &mut self.scratch.parse,
        );
        if diagnostics.has_errors() {
            return Err(diagnostics.into_vec());
        }
        let input = Input {
            text: source,
            source: Self::SOURCE,
            tokens,
            ast,
            interner: &self.interner,
            hosts: &self.hosts,
        };
        let checked = eitri_check::check(&input, &mut diagnostics, &mut self.scratch.check);
        if diagnostics.has_errors() {
            return Err(diagnostics.into_vec());
        }
        let program = if SIMPLE {
            eitri_lower::lower_simple(&input, checked, &mut self.scratch.lower, self.keep_ir)
        } else {
            eitri_lower::lower(&input, checked, &mut self.scratch.lower, self.keep_ir)
        }
        .map_err(|diagnostic| vec![diagnostic])?;
        // Compiler-produced code is trusted in release builds; debug builds
        // and tests verify every function, so a lowering bug surfaces as a
        // `bug:` diagnostic instead of a wrong answer.
        if cfg!(debug_assertions) {
            verify(&program, &self.hosts)?;
        }
        Ok(Compiled::new(program, &self.hosts, &self.interner))
    }

    fn check_bindings(&self, compiled: &Compiled) -> Result<(), FaultReport> {
        for binding in &compiled.host_bindings {
            let matches = (binding.index as usize) < self.hosts.function_count() && {
                let function = self.hosts.function(HostFnId::from_u32(binding.index));
                self.hosts.module(function.module).path == binding.module
                    && self.interner.resolve(function.name) == binding.name
                    && self.hosts.signature(HostFnId::from_u32(binding.index)) == &binding.signature
            };
            if !matches {
                return Err(FaultReport {
                    code: Some(Code::HostBindingMismatch),
                    message: format!(
                        "host binding at index {} does not match `{}.{}`",
                        binding.index, binding.module, binding.name
                    ),
                    trace: Vec::new(),
                });
            }
        }
        Ok(())
    }

    /// Execute compiled top-level code once. Every host index used by the
    /// artifact must have the same module, function name and checked signature
    /// in this registry. Otherwise returns [`Code::HostBindingMismatch`] before
    /// execution or changing the run count. Host calls go to `host`, by
    /// [`Context::host_index`]. Validation allocates nothing on success and is
    /// O(name bytes + encoded signature bytes) across used bindings, O(1)
    /// extra space. Each cached encoding is bounded by u32::MAX bytes;
    /// for P scalar parameters its length is P+5. No local type IDs compare.
    /// Reentry borrows this validated program and registry for the whole run.
    pub fn execute(&mut self, compiled: &Compiled, host: &mut dyn Host) -> Result<(), FaultReport> {
        self.check_bindings(compiled)?;
        compiled.runs.set(compiled.runs.get().saturating_add(1));
        let program = &compiled.program;
        let run = if self.threaded {
            eitri_vm::run_threaded
        } else {
            eitri_vm::run
        };
        run(
            &program.functions,
            program.entry,
            &[],
            host,
            self.limits,
            &mut self.scratch.vm,
        )
        .map(drop)
        .map_err(|fault| report(program, fault, &self.hosts, &self.interner))
    }
}

impl Context {
    /// Assemble `.eits` text ([`Context::disassemble`]'s output, or written
    /// by hand) into code this context can execute. Host functions are
    /// resolved by qualified name against the registered host modules, and
    /// every function is verified: the text is untrusted. Diagnostics
    /// refer to `text` as [`Context::SOURCE`], and so do faults' traces.
    pub fn assemble(&mut self, text: &str) -> Result<Compiled, Vec<Diagnostic>> {
        let parsed = text::parse(text).map_err(|error| {
            let code = if error.mismatch {
                Code::AsmMismatch
            } else {
                Code::AsmSyntax
            };
            vec![
                Diagnostic::error(code, error.message)
                    .with_label(Label::primary(Self::SOURCE, error.span)),
            ]
        })?;
        let mismatch = |span: Span, message: String| {
            vec![
                Diagnostic::error(Code::AsmMismatch, message)
                    .with_label(Label::primary(Self::SOURCE, span)),
            ]
        };
        // Each declared host function, as a registry index.
        let registered = eitri_lower::host_signatures(&self.hosts);
        let mut indices = Vec::with_capacity(parsed.hosts.len());
        for (decl, &span) in parsed.hosts.iter().zip(&parsed.host_spans) {
            let index = decl
                .name
                .rsplit_once('.')
                .and_then(|(module, name)| self.host_index(module, name));
            let Some(index) = index else {
                return Err(mismatch(
                    span,
                    format!("host function `{}` is not registered", decl.name),
                ));
            };
            if registered[index as usize] != decl.signature {
                return Err(mismatch(
                    span,
                    format!(
                        "`{}` is declared with a signature other than the registered one",
                        decl.name
                    ),
                ));
            }
            indices.push(index);
        }
        let mut functions: IndexVec<FunctionId, _> = IndexVec::from_raw(parsed.functions);
        // Declaration order to registry indices, in the code and its IR.
        let remap = |site: &mut CallSite| {
            if let CallTarget::Host(index) = &mut site.target {
                *index = indices[*index as usize];
            }
        };
        for function in functions.as_mut_slice() {
            function.sites.iter_mut().for_each(remap);
        }
        let mut bodies = parsed.bodies;
        for item in bodies.iter_mut().flat_map(|body| body.items.iter_mut()) {
            if let Insn::Call(site) = &mut item.insn {
                remap(site);
            }
        }
        let entry = FunctionId::from_u32(0);
        let entry_ok = functions.get(entry).is_some_and(|f| {
            &*f.name == text::MODULE
                && f.signature.params.is_empty()
                && f.signature.results.is_empty()
        });
        if !entry_ok {
            let span = parsed.function_spans.first().copied().unwrap_or_default();
            return Err(mismatch(
                span,
                format!(
                    "the first function must be `define void @{}()`, the top-level code",
                    text::MODULE
                ),
            ));
        }
        let program = Program {
            functions,
            entry,
            bodies: Some(IndexVec::from_raw(bodies)),
        };
        let signatures = program.signatures();
        let callees = Callees {
            functions: &signatures,
            hosts: &registered,
        };
        for f in program.functions.iter() {
            if let Err(error) = eitri_bytecode::verify(f, &callees) {
                let mut diagnostic = Diagnostic::error(
                    Code::AsmRejected,
                    format!("`{}` is rejected by the verifier: {error}", f.name),
                );
                if let Some(span) = f.span_at(error.pc) {
                    diagnostic = diagnostic.with_label(Label::primary(Self::SOURCE, span));
                }
                return Err(vec![diagnostic]);
            }
        }
        Ok(Compiled::new(program, &self.hosts, &self.interner))
    }

    /// `compiled` as `.eits` text, with host functions named by this
    /// context's registry; [`Context::assemble`] turns it back into the
    /// same code. `None` when `compiled` kept no logical instructions: it
    /// was compiled without [`Context::set_keep_ir`]. Also returns `None` if
    /// this registry does not match the artifact's used host bindings.
    pub fn disassemble(&self, compiled: &Compiled, options: PrintOptions) -> Option<String> {
        self.check_bindings(compiled).ok()?;
        let bodies = compiled.program.bodies.as_ref()?;
        let signatures = eitri_lower::host_signatures(&self.hosts);
        let hosts: Vec<HostDecl> = signatures
            .into_iter()
            .enumerate()
            .map(|(index, signature)| {
                let host = self.hosts.function(HostFnId::from_u32(index as u32));
                let module = &self.hosts.module(host.module).path;
                HostDecl {
                    name: format!("{module}.{}", self.interner.resolve(host.name)),
                    signature,
                }
            })
            .collect();
        Some(text::print(
            compiled.program.functions.as_slice(),
            bodies.as_slice(),
            &hosts,
            options,
        ))
    }

    /// The tokens of `source`, one per line: byte range, kind, and text.
    pub fn dump_tokens(&mut self, source: &str) -> Result<String, Vec<Diagnostic>> {
        self.interner.reset();
        let mut diagnostics = Diagnostics::new(DIAGNOSTIC_LIMIT);
        let tokens = eitri_syntax::lex(
            source,
            Self::SOURCE,
            &mut self.interner,
            &mut diagnostics,
            &mut self.scratch.lex,
        );
        if diagnostics.has_errors() {
            return Err(diagnostics.into_vec());
        }
        Ok(eitri_syntax::dump::tokens(source, tokens))
    }

    /// The checked syntax tree of `source`, in the style of
    /// `clang -ast-dump`: kinds, `<line:col>` ranges, and types. Rejected
    /// only for lexical and syntax errors; an expression the checker could
    /// not type shows `'<error>'`. `color` adds terminal colors.
    pub fn dump_ast(&mut self, source: &str, color: bool) -> Result<String, Vec<Diagnostic>> {
        self.interner.reset();
        let mut diagnostics = Diagnostics::new(DIAGNOSTIC_LIMIT);
        let tokens = eitri_syntax::lex(
            source,
            Self::SOURCE,
            &mut self.interner,
            &mut diagnostics,
            &mut self.scratch.lex,
        );
        if diagnostics.has_errors() {
            return Err(diagnostics.into_vec());
        }
        let ast = eitri_syntax::parse(
            source,
            Self::SOURCE,
            tokens,
            ModuleKind::Script,
            &mut diagnostics,
            &mut self.scratch.parse,
        );
        if diagnostics.has_errors() {
            return Err(diagnostics.into_vec());
        }
        let input = Input {
            text: source,
            source: Self::SOURCE,
            tokens,
            ast,
            interner: &self.interner,
            hosts: &self.hosts,
        };
        // Check errors do not stop the dump: they show as `'<error>'`.
        let mut ignored = Diagnostics::new(DIAGNOSTIC_LIMIT);
        let checked = eitri_check::check(&input, &mut ignored, &mut self.scratch.check);
        Ok(eitri_check::dump::ast(
            &eitri_check::dump::Input {
                text: source,
                tokens,
                ast,
                checked,
                interner: &self.interner,
                hosts: &self.hosts,
            },
            color,
        ))
    }
}

/// A module compiled by [`Context::compile`]: owned bytecode, independent
/// of the context's scratch, so it can be executed any number of times.
pub struct Compiled {
    program: Program,
    host_bindings: Vec<HostBinding>,
    /// Executions so far, for [`Context::should_optimize`]. A `Cell`: the
    /// count changes while the code is shared for execution.
    runs: Cell<u32>,
    optimized: bool,
}

/// Only referenced host indexes, sorted and deduplicated. Owned spellings
/// survive interner resets and destruction of the compiling context.
struct HostBinding {
    index: u32,
    module: String,
    name: String,
    signature: PortableSignature,
}

impl Compiled {
    fn new(program: Program, hosts: &Hosts, interner: &Interner) -> Self {
        // S call sites: O(S log S) time and O(S) temporary space; retained
        // metadata is O(U + name bytes + parameter types) for U used indexes.
        let mut indices: Vec<u32> = program
            .functions
            .iter()
            .flat_map(|f| &f.sites)
            .filter_map(|site| match site.target {
                CallTarget::Host(index) => Some(index),
                CallTarget::Script(_) => None,
            })
            .collect();
        indices.sort_unstable();
        indices.dedup();
        let host_bindings = indices
            .into_iter()
            .map(|index| {
                let function = hosts.function(HostFnId::from_u32(index));
                HostBinding {
                    index,
                    module: hosts.module(function.module).path.clone(),
                    name: interner.resolve(function.name).to_owned(),
                    signature: hosts.signature(HostFnId::from_u32(index)).clone(),
                }
            })
            .collect();
        Compiled {
            program,
            host_bindings,
            runs: Cell::new(0),
            optimized: false,
        }
    }

    /// How many times it has been executed.
    pub fn runs(&self) -> u32 {
        self.runs.get()
    }

    /// Whether [`Context::optimize`] has replaced its code.
    pub fn is_optimized(&self) -> bool {
        self.optimized
    }

    /// Functions, including the top-level `<module>`.
    pub fn function_count(&self) -> usize {
        self.program.functions.len()
    }

    /// Bytes of instruction words.
    pub fn code_bytes(&self) -> usize {
        self.program
            .functions
            .iter()
            .map(|f| f.code.len() * 4)
            .sum()
    }

    /// Bytes of everything else the functions carry: constants, call
    /// sites, literal types, instruction-start bits, source spans, names,
    /// signatures, and recorded host bindings (heap parts only). Scalar
    /// function type vectors and owned portable host signatures are counted.
    /// Artifacts currently retain no layout stores; their layout bytes are zero.
    pub fn metadata_bytes(&self) -> usize {
        self.host_bindings.capacity() * size_of::<HostBinding>()
            + self
                .host_bindings
                .iter()
                .map(|binding| {
                    binding.module.capacity()
                        + binding.name.capacity()
                        + binding.signature.heap_bytes()
                })
                .sum::<usize>()
            + self
                .program
                .functions
                .iter()
                .map(|f| {
                    size_of_val(&*f.consts)
                        + size_of_val(&*f.sites)
                        + f.roots.heap_bytes()
                        + size_of_val(&*f.literal_types)
                        + size_of_val(&*f.starts)
                        + size_of_val(&*f.spans)
                        + f.name.len()
                        + f.signature.params.capacity() * size_of::<eitri_bytecode::ValueType>()
                        + f.signature.results.capacity() * size_of::<eitri_bytecode::ValueType>()
                })
                .sum::<usize>()
    }

    /// The largest frame, in 8-byte slots.
    pub fn max_frame_slots(&self) -> u32 {
        self.program
            .functions
            .iter()
            .map(|f| f.frame_size)
            .max()
            .unwrap_or(0)
    }
}

fn verify(program: &Program, hosts: &Hosts) -> Result<(), Vec<Diagnostic>> {
    let functions = program.signatures();
    let hosts = eitri_lower::host_signatures(hosts);
    let callees = Callees {
        functions: &functions,
        hosts: &hosts,
    };
    for f in program.functions.iter() {
        if let Err(error) = eitri_bytecode::verify(f, &callees) {
            let message = format!("lowering produced invalid code in `{}`: {error:?}", f.name);
            return Err(vec![Diagnostic::bug(message)]);
        }
    }
    Ok(())
}

/// Resolve a fault's trace to names and source spans: the cold path.
fn report(program: &Program, fault: Fault, hosts: &Hosts, interner: &Interner) -> FaultReport {
    let trace = fault
        .trace
        .iter()
        .map(|location| match *location {
            Location::Script { function, pc } => {
                let function = &program.functions[function];
                TraceFrame {
                    function: function.name.to_string(),
                    span: function.span_at(pc),
                }
            }
            Location::Host(index) => {
                let host = hosts.function(HostFnId::from_u32(index));
                let module = &hosts.module(host.module).path;
                TraceFrame {
                    function: format!("{module}.{}", interner.resolve(host.name)),
                    span: None,
                }
            }
        })
        .collect();
    let message = match fault.kind {
        eitri_vm::FaultKind::Host(message) => message,
        kind => kind.message().to_owned(),
    };
    FaultReport {
        code: None,
        message,
        trace,
    }
}

#[cfg(test)]
mod root_optimizer_tests {
    use super::*;
    use eitri_bytecode::{RootMap, RootMapId};
    const SOURCE: &str = "fn id(n: i64) -> i64 {\n return n\n}\nlet x = id(1)\n";

    #[test]
    fn optimize_accepts_equivalent_renumbered_root_maps() {
        let mut context = Context::new();
        let mut compiled = context.compile(SOURCE).unwrap();
        for f in compiled.program.functions.as_mut_slice().iter_mut() {
            f.roots.maps.insert(0, RootMap::default());
            f.roots.entry = RootMapId(f.roots.entry.0 + 1);
            for (_, state) in &mut f.roots.safepoints {
                state.map.0 += 1;
            }
            for call in &mut f.roots.calls {
                call.pre.map.0 += 1;
                call.suspended.map.0 += 1;
                call.post.map.0 += 1;
            }
        }
        context.optimize(&mut compiled, SOURCE).unwrap();
        assert!(compiled.is_optimized());
    }

    #[test]
    fn invalid_root_metadata_rejects_optimization_without_losing_ir() {
        let mut context = Context::new();
        context.set_keep_ir(true);
        let mut compiled = context.compile(SOURCE).unwrap();
        compiled.program.functions.as_mut_slice()[0].roots.entry = RootMapId(99);
        assert!(matches!(
            context.optimize_kept(&mut compiled),
            Err(OptimizeError::InvalidBytecode(_))
        ));
        assert!(compiled.program.bodies.is_some());
        assert!(!compiled.optimized);
    }

    #[test]
    fn metadata_counts_retained_signature_capacity() {
        let mut context = Context::new();
        let mut compiled = context.compile(SOURCE).unwrap();
        let before = compiled.metadata_bytes();
        let sig = &mut compiled.program.functions.as_mut_slice()[0].signature;
        let old = sig.params.capacity();
        sig.params.reserve(32);
        let added = (sig.params.capacity() - old) * size_of::<eitri_bytecode::ValueType>();
        assert!(added > 0);
        assert_eq!(compiled.metadata_bytes(), before + added);
    }

    #[test]
    fn optimizer_keeps_nontrivial_root_states_unchanged() {
        let mut context = Context::new();
        context.set_keep_ir(true);
        let mut compiled = context.compile(SOURCE).unwrap();
        let function = compiled
            .program
            .functions
            .as_mut_slice()
            .iter_mut()
            .find(|f| !f.sites.is_empty())
            .unwrap();
        function.roots.calls[0]
            .pre
            .initialized
            .push(function.sites[0].args);
        let original = function.clone();
        let report = context.optimize_kept(&mut compiled).unwrap();
        assert!(report.skipped >= 1);
        assert!(compiled.program.functions.iter().any(|f| *f == original));
    }
}
