use eitri_base::{IndexVec, Interner, SourceId, Span};
use eitri_bytecode::text::{self, HostDecl, PrintOptions};
use eitri_bytecode::{CallTarget, Callees, FunctionId};
use eitri_check::{CheckScratch, HostFnId, Hosts, Input};
use eitri_diag::{Code, Diagnostic, Diagnostics, Label};
use eitri_lower::{LowerScratch, Program};
use eitri_syntax::{LexScratch, ModuleKind, ParseScratch};
use eitri_vm::{Fault, Host, Limits, Location, VmScratch};

/// Diagnostics kept per run before the rest are only counted (T047).
const DIAGNOSTIC_LIMIT: usize = 100;

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

/// A fault and its trace, innermost first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaultReport {
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
        let mut diagnostic = Diagnostic::fault(self.message.as_str());
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
        let program = eitri_lower::lower(&input, checked, &mut self.scratch.lower)
            .map_err(|diagnostic| vec![diagnostic])?;
        // Compiler-produced code is trusted in release builds; debug builds
        // and tests verify every function, so a lowering bug surfaces as a
        // `bug:` diagnostic instead of a wrong answer.
        if cfg!(debug_assertions) {
            verify(&program, &self.hosts)?;
        }
        Ok(Compiled { program })
    }

    /// Execute compiled top-level code once. `compiled` must come from this
    /// context, since host calls are numbered by its registry. Host calls go
    /// to `host`, by [`Context::host_index`].
    pub fn execute(&mut self, compiled: &Compiled, host: &mut dyn Host) -> Result<(), FaultReport> {
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
        for function in functions.as_mut_slice() {
            for site in function.sites.iter_mut() {
                if let CallTarget::Host(index) = &mut site.target {
                    *index = indices[*index as usize];
                }
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
        let program = Program { functions, entry };
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
        Ok(Compiled { program })
    }

    /// `compiled` as `.eits` text, with host functions named by this
    /// context's registry. [`Context::assemble`] turns it back into the
    /// same code.
    pub fn disassemble(&self, compiled: &Compiled, options: PrintOptions) -> String {
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
        text::print(compiled.program.functions.as_slice(), &hosts, options)
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

    /// The syntax tree of `source`, indented, with byte ranges.
    pub fn dump_ast(&mut self, source: &str) -> Result<String, Vec<Diagnostic>> {
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
        Ok(eitri_syntax::dump::ast(source, tokens, ast))
    }
}

/// A module compiled by [`Context::compile`]: owned bytecode, independent
/// of the context's scratch, so it can be executed any number of times.
pub struct Compiled {
    program: Program,
}

impl Compiled {
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
    /// and signatures (heap parts only).
    pub fn metadata_bytes(&self) -> usize {
        self.program
            .functions
            .iter()
            .map(|f| {
                size_of_val(&*f.consts)
                    + size_of_val(&*f.sites)
                    + size_of_val(&*f.literal_types)
                    + size_of_val(&*f.starts)
                    + size_of_val(&*f.spans)
                    + f.name.len()
                    + f.signature.params.len()
                    + f.signature.results.len()
            })
            .sum()
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
    FaultReport { message, trace }
}
