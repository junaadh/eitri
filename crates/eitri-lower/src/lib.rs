//! Lowering (T009): checked syntax to typed bytecode functions.
//!
//! One bytecode function per script `fn`, plus `<module>` for the top-level
//! statements (function 0, the entry). Frames follow the bytecode
//! convention: results, then parameters, then bindings and temporaries.
//!
//! - Bindings get a slot when declared and keep it to the end of their
//!   block; temporaries come from a stack-ordered counter released after
//!   every statement, so slots stay low.
//! - A name used as an operand is read from its binding's slot directly,
//!   without a copy. Sound because nothing in the first slice can change a
//!   local in the middle of an expression; closures (T019) must revisit this.
//! - Operands are evaluated left to right and nothing is constant-folded,
//!   so faults happen at run time after every earlier effect.
//! - Operator chains, `&&`/`||` chains, and `else if` chains are walked
//!   iteratively; recursion only follows parenthesized nesting, which the
//!   parser bounds.
//! - Conditions branch directly (compare-and-branch on the complement), so
//!   `if a < b` is one instruction, not a `CSET` plus a test.
//! - A `while` loop is top-tested and its back edge is the `J` after the
//!   body, so the budget is polled after the body and before the condition,
//!   as the first slice specifies.
//!
//! `lower_simple` retains the comparison baseline: binding operands are
//! copied, arithmetic uses registers, and comparisons materialize before
//! branching. The choice is a const parameter, so the default lowering has
//! no runtime mode test. Both modes retain destination passing and reuse.
//!
//! Time and memory are linear in the module's syntax.

use eitri_base::{Idx, IndexVec, Span};
use eitri_bytecode::ir::Body;
use eitri_bytecode::spec::{Cond, Test};
use eitri_bytecode::{
    AsmError, Assembler, CallSite, CallTarget, Function, FunctionId, Label, Op, Operands,
    Signature, SlotRange, ValueType, imm_complement,
};
use eitri_check::{BindingId, Checked, HostFnId, Hosts, Input, Res, Ty};
use eitri_diag::{Code, Diagnostic, Label as DiagLabel};
use eitri_syntax::ast::{
    AssignOp, BinaryOp, Block, Else, Expr, ExprId, FnId, IfExpr, IfId, Item, Stmt, StmtId, UnaryOp,
};

/// The top-level code: always function 0.
pub const ENTRY: FunctionId = FunctionId::from_u32(0);

/// One compiled module. Owned by the run that compiled it.
pub struct Program {
    pub functions: IndexVec<FunctionId, Function>,
    pub entry: FunctionId,
    /// Owned layouts. Scalar-only programs need no layout table. Managed
    /// bytecode must check its required storage domain before using this table;
    /// IDs are local to it and must never be compared across programs.
    pub layouts: Option<eitri_types::LayoutStore>,
    /// Each function's logical instructions (`.eits` in memory), when
    /// lowering was asked to keep them.
    pub bodies: Option<IndexVec<FunctionId, Body>>,
}

impl Program {
    /// Every function's signature, for the verifier.
    pub fn signatures(&self) -> IndexVec<FunctionId, Signature> {
        Self::signatures_of(&self.functions)
    }

    pub fn signatures_of(
        functions: &IndexVec<FunctionId, Function>,
    ) -> IndexVec<FunctionId, Signature> {
        IndexVec::from_raw(functions.iter().map(|f| f.signature.clone()).collect())
    }
}

/// Lowering's reusable working memory.
#[derive(Default)]
pub struct LowerScratch {
    asm: Assembler,
    /// Used instead of `asm` when the IR is kept: a separate instantiation,
    /// so first-run lowering contains no recording code.
    recording: Assembler<true>,
    slots: IndexVec<BindingId, u32>,
    stack: Vec<ExprId>,
    loops: Vec<(Label, Label)>,
}

impl LowerScratch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-reserve lowering scratch for a budgeted compilation. The caller
    /// supplies the token and binding counts and selects the recording path.
    /// Output artifacts and managed root analysis require separate charges.
    pub fn reserve_budgeted(
        &mut self,
        tokens: usize,
        bindings: usize,
        recording: bool,
        budget: &mut eitri_base::MemoryBudget,
    ) -> Result<(), eitri_base::OutOfMemory> {
        let instructions = tokens
            .checked_mul(16)
            .and_then(|n| n.checked_add(64))
            .ok_or(eitri_base::OutOfMemory)?;
        if recording {
            self.recording.reserve_budgeted(instructions, budget)?;
        } else {
            self.asm.reserve_budgeted(instructions, budget)?;
        }
        self.slots
            .reserve_budgeted(bindings.saturating_sub(self.slots.len()), budget)?;
        let additional = tokens.saturating_sub(self.stack.len());
        budget.reserve_vec(&mut self.stack, additional)?;
        let additional = tokens.saturating_sub(self.loops.len());
        budget.reserve_vec(&mut self.loops, additional)?;
        Ok(())
    }

    /// Retained scratch with conservative assembler hash-table charges.
    pub fn budget_bytes(&self) -> Result<usize, eitri_base::OutOfMemory> {
        self.asm
            .budget_bytes()?
            .checked_add(self.recording.budget_bytes()?)
            .and_then(|n| n.checked_add(self.slots.capacity() * size_of::<u32>()))
            .and_then(|n| n.checked_add(self.stack.capacity() * size_of::<ExprId>()))
            .and_then(|n| n.checked_add(self.loops.capacity() * size_of::<(Label, Label)>()))
            .ok_or(eitri_base::OutOfMemory)
    }

    /// Transfer the active compilation budget to the selected assembler.
    pub fn set_memory_budget(&mut self, budget: Option<eitri_base::MemoryBudget>, recording: bool) {
        if recording {
            self.recording.set_memory_budget(budget);
        } else {
            self.asm.set_memory_budget(budget);
        }
    }

    /// Recover the budget and the artifact bytes already charged by assembly.
    pub fn take_memory_budget(
        &mut self,
        recording: bool,
    ) -> (Option<eitri_base::MemoryBudget>, usize) {
        if recording {
            self.recording.take_memory_budget()
        } else {
            self.asm.take_memory_budget()
        }
    }

    /// Retained vector allocations, excluding the assemblers' hash tables.
    pub fn vector_bytes(&self) -> usize {
        self.asm.vector_bytes()
            + self.recording.vector_bytes()
            + self.slots.capacity() * size_of::<u32>()
            + self.stack.capacity() * size_of::<ExprId>()
            + self.loops.capacity() * size_of::<(Label, Label)>()
    }
}

pub fn value_type(ty: Ty) -> ValueType {
    match ty {
        Ty::I64 => ValueType::I64,
        Ty::I8 => ValueType::I8,
        Ty::U8 => ValueType::U8,
        Ty::U64 => ValueType::U64,
        Ty::F32 => ValueType::F32,
        Ty::F64 => ValueType::F64,
        Ty::Char => ValueType::Char,
        Ty::Bool => ValueType::Bool,
        Ty::String => ValueType::String,
        Ty::Class(id) => ValueType::Record(id.as_u32()),
        Ty::OptionalString => ValueType::NullableString,
        Ty::OptionalClass(id) => ValueType::NullableRecord(id.as_u32()),
        Ty::FindOffset => ValueType::I64,
        _ => {
            unreachable!("checked code has only value types here")
        }
    }
}

fn string_format_op(ty: Ty) -> Op {
    match ty {
        Ty::I64 => Op::StrFmtI64,
        Ty::I8 => Op::StrFmtI8,
        Ty::U8 => Op::StrFmtU8,
        Ty::U64 => Op::StrFmtU64,
        Ty::F32 => Op::StrFmtF32,
        Ty::F64 => Op::StrFmtF64,
        Ty::Bool => Op::StrFmtBool,
        Ty::Char => Op::StrFmtChar,
        Ty::String => Op::Mov,
        _ => unreachable!("checked formattable type"),
    }
}

fn signature(params: &[Ty], ret: Ty) -> Signature {
    Signature {
        params: params.iter().map(|&ty| value_type(ty)).collect(),
        results: if ret == Ty::Void {
            Vec::new()
        } else {
            vec![value_type(ret)]
        },
    }
}

/// Bytecode signatures of every registered host function, by host index.
pub fn host_signatures(hosts: &Hosts) -> Vec<Signature> {
    (0..hosts.function_count())
        .map(|index| {
            let host = hosts.function(HostFnId::new(index));
            signature(&host.params, host.ret)
        })
        .collect()
}

/// Lower a checked module. The only error is an implementation limit
/// (`E0002`): a function needing more than 65,536 slots or too much code.
/// With `keep_ir`, the assembler also records each function's logical
/// instructions for printing or re-optimizing; a first run passes `false`.
pub fn lower(
    input: &Input<'_>,
    checked: &Checked,
    scratch: &mut LowerScratch,
    keep_ir: bool,
) -> Result<Program, Diagnostic> {
    lower_mode::<false>(input, checked, scratch, keep_ir)
}

/// Retained simple lowering baseline (T009): copy binding operands, use
/// register arithmetic, and materialize comparisons before testing them.
/// Keeps destination passing and stack-ordered temporary reuse. It has the
/// same source semantics and limits as `lower`, but may need more slots.
pub fn lower_simple(
    input: &Input<'_>,
    checked: &Checked,
    scratch: &mut LowerScratch,
    keep_ir: bool,
) -> Result<Program, Diagnostic> {
    lower_mode::<true>(input, checked, scratch, keep_ir)
}

fn lower_mode<const SIMPLE: bool>(
    input: &Input<'_>,
    checked: &Checked,
    scratch: &mut LowerScratch,
    keep_ir: bool,
) -> Result<Program, Diagnostic> {
    scratch.slots.clear();
    scratch.slots.resize(checked.bindings.len(), u32::MAX);
    scratch.stack.clear();
    scratch.loops.clear();
    if keep_ir {
        let lowerer = Lowerer::<_, SIMPLE> {
            input,
            checked,
            asm: &mut scratch.recording,
            slots: &mut scratch.slots,
            stack: &mut scratch.stack,
            plans: Vec::new(),
            loops: &mut scratch.loops,
            mutation_result: None,
            current_ret: Ty::Void,
            default_depth: 0,
            default_expansions: 0,
            top: 0,
        };
        lower_with(lowerer)
    } else {
        let lowerer = Lowerer::<_, SIMPLE> {
            input,
            checked,
            asm: &mut scratch.asm,
            slots: &mut scratch.slots,
            stack: &mut scratch.stack,
            plans: Vec::new(),
            loops: &mut scratch.loops,
            mutation_result: None,
            current_ret: Ty::Void,
            default_depth: 0,
            default_expansions: 0,
            top: 0,
        };
        lower_with(lowerer)
    }
}

/// Lower every function; with a recording assembler, keep each body.
fn lower_with<const R: bool, const SIMPLE: bool>(
    mut lowerer: Lowerer<'_, '_, R, SIMPLE>,
) -> Result<Program, Diagnostic> {
    let (input, ast) = (lowerer.input, lowerer.input.ast);
    let capacity = ast.fns.len() + 1;
    let mut bodies = if R {
        Some(IndexVec::from_raw(
            lowerer
                .asm
                .artifact_vec(capacity)
                .map_err(|error| lowering_error(error, input, "<module>", Span::default()))?,
        ))
    } else {
        None
    };
    let mut functions = IndexVec::from_raw(
        lowerer
            .asm
            .artifact_vec(capacity)
            .map_err(|error| lowering_error(error, input, "<module>", Span::default()))?,
    );
    let module = lowerer
        .module()
        .map_err(|error| lowering_error(error, input, "<module>", Span::default()))?;
    functions.push(module);
    if let Some(bodies) = &mut bodies {
        bodies.push(
            lowerer
                .asm
                .take_recorded()
                .expect("recorded while lowering"),
        );
    }
    for (id, decl) in ast.fns.iter_enumerated() {
        let function = lowerer.function(id).map_err(|error| {
            let name = input.interner.resolve(input.tokens.symbol(decl.name));
            lowering_error(error, input, name, input.tokens.span(decl.name))
        })?;
        functions.push(function);
        if let Some(bodies) = &mut bodies {
            bodies.push(
                lowerer
                    .asm
                    .take_recorded()
                    .expect("recorded while lowering"),
            );
        }
    }
    let has_classes = ast.structs.iter().any(|decl| decl.is_class)
        || ast
            .enums
            .iter()
            .any(|decl| ast.enum_cases[decl.cases].iter().any(|case| case.indirect));
    let mut record_shapes = lowerer
        .asm
        .temporary_vec(if has_classes {
            ast.structs.len() + lowerer.checked.enum_case_types.len()
        } else {
            0
        })
        .map_err(|error| lowering_error(error, input, "<module>", Span::default()))?;
    if has_classes {
        for (id, decl) in ast.structs.iter_enumerated() {
            let fields = lowerer.checked.struct_fields[id];
            let mut shape = lowerer
                .asm
                .temporary_vec(if decl.is_class {
                    lowerer.checked.struct_widths[id] as usize
                } else {
                    0
                })
                .map_err(|error| lowering_error(error, input, "<module>", Span::default()))?;
            if decl.is_class {
                for index in 0..lowerer.checked.param_types[fields].len() {
                    let ty = lowerer.checked.param_types[fields][index];
                    lowerer.flatten_type(ty, &mut shape).map_err(|error| {
                        lowering_error(error, input, "<module>", Span::default())
                    })?;
                }
            }
            record_shapes.push(shape.into_boxed_slice());
        }
        for (id, decl) in ast.enums.iter_enumerated() {
            let start = lowerer.checked.enum_starts[id] as usize;
            for (index, case) in ast.enum_cases[decl.cases].iter().enumerate() {
                let fields = lowerer.checked.enum_case_types[start + index];
                let mut shape = lowerer
                    .asm
                    .temporary_vec(0)
                    .map_err(|error| lowering_error(error, input, "<module>", Span::default()))?;
                if case.indirect {
                    for field in 0..lowerer.checked.param_types[fields].len() {
                        let ty = lowerer.checked.param_types[fields][field];
                        lowerer.flatten_type(ty, &mut shape).map_err(|error| {
                            lowering_error(error, input, "<module>", Span::default())
                        })?;
                    }
                }
                record_shapes.push(shape.into_boxed_slice());
            }
        }
        for function in functions.as_mut_slice() {
            let mut shapes = lowerer
                .asm
                .artifact_vec(record_shapes.len())
                .map_err(|error| lowering_error(error, input, &function.name, Span::default()))?;
            for shape in &record_shapes {
                let mut fields = lowerer.asm.artifact_vec(shape.len()).map_err(|error| {
                    lowering_error(error, input, &function.name, Span::default())
                })?;
                fields.extend_from_slice(shape);
                shapes.push(fields.into_boxed_slice());
            }
            function.record_shapes = shapes.into_boxed_slice();
            function.record_class_count = ast.structs.len() as u32;
        }
    }
    if has_classes
        || !lowerer.checked.strings.is_empty()
        || !lowerer.checked.enum_case_types.is_empty()
        || !lowerer.plans.is_empty()
        || functions.iter().any(|f| {
            f.signature
                .params
                .iter()
                .chain(&f.signature.results)
                .any(|t| t.is_managed())
        })
        || functions
            .iter()
            .flat_map(|f| &f.sites)
            .any(|site| match site.target {
                CallTarget::Host(index) => {
                    let host = input.hosts.function(HostFnId::from_u32(index));
                    host.ret == Ty::String || host.params.contains(&Ty::String)
                }
                CallTarget::Script(_) => false,
            })
    {
        let temporary_error = |error| lowering_error(error, input, "<module>", Span::default());
        let mut signature_vec = lowerer
            .asm
            .temporary_vec(functions.len())
            .map_err(temporary_error)?;
        for function in functions.iter() {
            let mut params = lowerer
                .asm
                .temporary_vec(function.signature.params.len())
                .map_err(temporary_error)?;
            params.extend_from_slice(&function.signature.params);
            let mut results = lowerer
                .asm
                .temporary_vec(function.signature.results.len())
                .map_err(temporary_error)?;
            results.extend_from_slice(&function.signature.results);
            signature_vec.push(Signature { params, results });
        }
        let signatures = IndexVec::from_raw(signature_vec);
        let mut hosts = lowerer
            .asm
            .temporary_vec(input.hosts.function_count())
            .map_err(temporary_error)?;
        for index in 0..input.hosts.function_count() {
            let host = input.hosts.function(HostFnId::new(index));
            let mut params = lowerer
                .asm
                .temporary_vec(host.params.len())
                .map_err(temporary_error)?;
            params.extend(host.params.iter().copied().map(value_type));
            let mut results = lowerer
                .asm
                .temporary_vec(usize::from(host.ret != Ty::Void))
                .map_err(temporary_error)?;
            if host.ret != Ty::Void {
                results.push(value_type(host.ret));
            }
            hosts.push(Signature { params, results });
        }
        let callees = eitri_bytecode::Callees {
            functions: &signatures,
            hosts: &hosts,
            record_shapes: &record_shapes,
            record_class_count: if has_classes {
                ast.structs.len() as u32
            } else {
                0
            },
        };
        for function in functions.as_mut_slice() {
            let mut pc = 0;
            let mut has_strings = false;
            while pc < function.code.len() {
                let decoded =
                    eitri_bytecode::decode(&function.code, pc).expect("assembled instruction");
                has_strings |= matches!(decoded.op, Op::StrNew | Op::StrInterpolate);
                pc += decoded.words as usize;
            }
            if has_strings {
                let mut strings = lowerer
                    .asm
                    .artifact_vec(lowerer.checked.strings.len() + lowerer.plans.len())
                    .map_err(|error| {
                        lowering_error(
                            error,
                            input,
                            &function.name,
                            function.span_at(0).unwrap_or_default(),
                        )
                    })?;
                for value in &lowerer.checked.strings {
                    strings.push(lowerer.asm.artifact_string(value).map_err(|error| {
                        lowering_error(
                            error,
                            input,
                            &function.name,
                            function.span_at(0).unwrap_or_default(),
                        )
                    })?);
                }
                for value in &lowerer.plans {
                    strings.push(lowerer.asm.artifact_string(value).map_err(|error| {
                        lowering_error(
                            error,
                            input,
                            &function.name,
                            function.span_at(0).unwrap_or_default(),
                        )
                    })?);
                }
                function.strings = strings.into_boxed_slice();
            }
            {
                let old_root_bytes = function.roots.heap_bytes();
                function.roots = lowerer
                    .asm
                    .string_roots(function, &callees)
                    .map_err(|error| {
                        let code = if error.kind == eitri_bytecode::ErrorKind::OutOfMemory {
                            Code::OutOfMemory
                        } else {
                            Code::Unsupported
                        };
                        let message = if code == Code::OutOfMemory {
                            "runtime memory limit exceeded"
                        } else {
                            "not supported yet: managed root metadata"
                        };
                        Diagnostic::error(code, message).with_label(DiagLabel::primary(
                            input.source,
                            function.span_at(0).unwrap_or_default(),
                        ))
                    })?;
                lowerer.asm.release_artifact_bytes(old_root_bytes);
            }
        }
        let mut temporary_bytes = signatures.capacity() * size_of::<Signature>()
            + hosts.capacity() * size_of::<Signature>();
        for signature in signatures.iter().chain(&hosts) {
            temporary_bytes += (signature.params.capacity() + signature.results.capacity())
                * size_of::<ValueType>();
        }
        drop(signatures);
        drop(hosts);
        lowerer.asm.release_temporary_bytes(temporary_bytes);
    }
    let shape_bytes = record_shapes.capacity() * size_of::<Box<[ValueType]>>()
        + record_shapes
            .iter()
            .map(|shape| size_of_val(shape.as_ref()))
            .sum::<usize>();
    drop(record_shapes);
    lowerer.asm.release_temporary_bytes(shape_bytes);
    Ok(Program {
        functions,
        entry: ENTRY,
        layouts: None,
        bodies,
    })
}

fn lowering_error(error: AsmError, input: &Input<'_>, name: &str, span: Span) -> Diagnostic {
    if error == AsmError::OutOfMemory {
        Diagnostic::error(Code::OutOfMemory, "runtime memory limit exceeded")
            .with_label(DiagLabel::primary(input.source, span))
    } else {
        too_large(input, name, span)
    }
}

fn too_large(input: &Input<'_>, name: &str, span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::Limit,
        format!("implementation limit: `{name}` needs more than 65,536 slots or too much code"),
    )
    .with_label(DiagLabel::primary(input.source, span))
}

/// Convert root-builder failures into a source-located implementation-limit
/// diagnostic. Callers must propagate every builder operation through this
/// boundary rather than discarding a failed snapshot.
pub fn root_build_diagnostic(
    error: eitri_bytecode::RootBuildError,
    source: eitri_base::SourceId,
    span: Span,
) -> Diagnostic {
    Diagnostic::error(
        Code::Limit,
        format!("implementation limit: root metadata could not be constructed ({error:?})"),
    )
    .with_label(DiagLabel::primary(source, span))
}

/// Finish and verify root metadata before publishing it to a function. A
/// failed build or verification leaves the original metadata intact. The
/// present verifier supports scalar executable facts; managed fact derivation
/// remains a prerequisite for managed lowering.
pub fn install_verified_roots(
    function: &mut Function,
    builder: eitri_bytecode::RootBuilder,
    pc_map: &[(u32, Vec<eitri_bytecode::SafepointPc>)],
    callees: &eitri_bytecode::Callees<'_>,
    source: eitri_base::SourceId,
    span: Span,
) -> Result<(), Diagnostic> {
    let roots = builder
        .finish(pc_map)
        .map_err(|e| root_build_diagnostic(e, source, span))?;
    let old = std::mem::replace(&mut function.roots, roots);
    if let Err(error) = eitri_bytecode::verify(function, callees) {
        function.roots = old;
        return Err(
            Diagnostic::bug(format!("invalid generated root metadata: {error}"))
                .with_label(DiagLabel::primary(source, span)),
        );
    }
    Ok(())
}

type Lowered<T> = Result<T, AsmError>;
type ClassFieldRoot = (
    Option<ExprId>,
    Option<BindingId>,
    eitri_syntax::ast::StructId,
    u32,
    ExprId,
);

struct Lowerer<'a, 'i, const R: bool, const SIMPLE: bool> {
    input: &'a Input<'i>,
    checked: &'a Checked,
    asm: &'a mut Assembler<R>,
    slots: &'a mut IndexVec<BindingId, u32>,
    stack: &'a mut Vec<ExprId>,
    plans: Vec<String>,
    loops: &'a mut Vec<(Label, Label)>,
    mutation_result: Option<(u32, u32, u32)>,
    current_ret: Ty,
    default_depth: u32,
    default_expansions: u32,
    /// The next free slot.
    top: u32,
}

impl<const R: bool, const SIMPLE: bool> Lowerer<'_, '_, R, SIMPLE> {
    // ---- functions -------------------------------------------------------

    fn module(&mut self) -> Lowered<Function> {
        self.top = 0;
        self.default_expansions = 0;
        let built = self.module_body();
        self.recover(built)
    }

    fn module_body(&mut self) -> Lowered<Function> {
        let ast = self.input.ast;
        for item in &ast.items {
            if let Item::Stmt(stmt) = *item {
                self.stmt(stmt)?;
            }
        }
        self.asm.ret(Span::default())?;
        self.asm.finish("<module>", Signature::default())
    }

    fn function(&mut self, id: FnId) -> Lowered<Function> {
        self.default_expansions = 0;
        let built = self.function_body(id);
        self.recover(built)
    }

    fn function_body(&mut self, id: FnId) -> Lowered<Function> {
        let (ast, checked) = (self.input.ast, self.checked);
        let decl = ast.fns[id];
        let types = checked.signatures[id];
        self.current_ret = types.ret;
        let parameter_types = &checked.param_types[types.params];
        let count: usize = parameter_types
            .iter()
            .map(|&ty| self.width(ty) as usize)
            .sum();
        let mut params = self.asm.artifact_vec(count)?;
        for &ty in parameter_types {
            self.flatten_type(ty, &mut params)?;
        }
        let return_width = if types.ret == Ty::Void {
            0
        } else {
            self.width(types.ret)
        };
        let self_ty = parameter_types.first().copied().filter(|_| decl.mutating);
        let self_width = self_ty.map_or(0, |ty| self.width(ty));
        let mut results = self
            .asm
            .artifact_vec((return_width + self_width) as usize)?;
        if types.ret != Ty::Void {
            self.flatten_type(types.ret, &mut results)?;
        }
        if let Some(ty) = self_ty {
            self.flatten_type(ty, &mut results)?;
        }
        let signature = Signature { params, results };
        let mut base = signature.param_base();
        for (binding, &ty) in checked.params[id].iter().zip(parameter_types) {
            self.slots[binding] = base;
            base += self.width(ty);
        }
        self.top = base;
        self.mutation_result = if decl.mutating {
            let binding = checked.params[id].iter().next().expect("method receiver");
            Some((return_width, self.slots[binding], self_width))
        } else {
            None
        };
        let body = decl.body.expect("script functions have bodies");
        self.statements(body)?;
        if types.ret == Ty::Void
            || matches!(types.ret, Ty::Result(id) if checked.result_types[id as usize].1 == Ty::Void)
        {
            if let Ty::Result(id) = types.ret {
                self.result_value(None, id, true, None, Some(0), body.span.shrink_to_end())?;
            }
            self.write_mutation_result(body.span.shrink_to_end())?;
            self.asm.ret(body.span.shrink_to_end())?;
        }
        self.mutation_result = None;
        let name = self
            .input
            .interner
            .resolve(self.input.tokens.symbol(decl.name));
        let qualified = if let Some(receiver) = decl.receiver {
            let owner = self
                .input
                .interner
                .resolve(self.input.tokens.symbol(receiver));
            let mut bytes = self.asm.temporary_vec(owner.len() + 1 + name.len())?;
            bytes.extend_from_slice(owner.as_bytes());
            bytes.push(b'.');
            bytes.extend_from_slice(name.as_bytes());
            Some(bytes)
        } else {
            None
        };
        let name = qualified.as_deref().map_or(name, |bytes| {
            std::str::from_utf8(bytes).expect("identifier UTF-8")
        });
        let mut function = self.asm.finish(name, signature)?;
        function.reentry_safe = !matches!(
            types.ret,
            Ty::FixedI64(_)
                | Ty::FixedString(_)
                | Ty::FixedScalar(_, _)
                | Ty::FixedClass(_, _)
                | Ty::FixedStruct(_, _)
                | Ty::FixedEnum(_, _)
                | Ty::Result(_)
                | Ty::Struct(_)
                | Ty::Enum(_)
                | Ty::Class(_)
                | Ty::OptionalClass(_)
                | Ty::OptionalString
        ) && types.ret.tagged_optional_inner().is_none()
            && !parameter_types.iter().any(|ty| {
                (matches!(
                    ty,
                    Ty::FixedI64(_)
                        | Ty::FixedString(_)
                        | Ty::FixedScalar(_, _)
                        | Ty::FixedClass(_, _)
                        | Ty::FixedStruct(_, _)
                        | Ty::FixedEnum(_, _)
                        | Ty::Result(_)
                        | Ty::Struct(_)
                        | Ty::Enum(_)
                        | Ty::Class(_)
                        | Ty::OptionalClass(_)
                        | Ty::OptionalString
                ) || ty.tagged_optional_inner().is_some())
            });
        Ok(function)
    }

    /// After a failure, empty the assembler so the next run starts clean.
    fn recover(&mut self, built: Lowered<Function>) -> Lowered<Function> {
        if built.is_err() {
            let _ = self.asm.finish("", Signature::default());
        }
        built
    }

    fn write_mutation_result(&mut self, span: Span) -> Lowered<()> {
        if let Some((out, source, width)) = self.mutation_result {
            for offset in 0..width {
                self.asm.emit(
                    Op::Mov,
                    Operands::abc((out + offset).into(), (source + offset).into(), 0),
                    span,
                )?;
            }
        }
        Ok(())
    }

    fn temp(&mut self) -> u32 {
        let slot = self.top;
        self.top += 1;
        slot
    }

    fn width(&self, ty: Ty) -> u32 {
        match ty {
            Ty::Result(id) => {
                let (error, success) = self.checked.result_types[id as usize];
                1 + self.width(error)
                    + if success == Ty::Void {
                        0
                    } else {
                        self.width(success)
                    }
            }
            Ty::FixedI64(size)
            | Ty::FixedString(size)
            | Ty::FixedScalar(size, _)
            | Ty::FixedClass(size, _) => u32::from(size),
            Ty::FixedStruct(size, id) => {
                u32::from(size).saturating_mul(self.checked.struct_widths[id])
            }
            Ty::FixedEnum(size, id) => u32::from(size).saturating_mul(self.checked.enum_widths[id]),
            Ty::Struct(id) => self.checked.struct_widths[id],
            Ty::Enum(id) => self.checked.enum_widths[id],
            Ty::OptionalStruct(id) => 1 + self.checked.struct_widths[id],
            Ty::OptionalEnum(id) => 1 + self.checked.enum_widths[id],
            ty if ty.tagged_optional_inner().is_some() => 2,
            _ => 1,
        }
    }

    fn flatten_type(&mut self, ty: Ty, output: &mut Vec<ValueType>) -> Lowered<()> {
        self.flatten_with_nullable(ty, false, output)
    }

    fn flatten_with_nullable(
        &mut self,
        ty: Ty,
        nullable: bool,
        output: &mut Vec<ValueType>,
    ) -> Lowered<()> {
        if let Some((size, inner)) = ty.fixed_inner()
            && !matches!(inner, Ty::Struct(_) | Ty::Enum(_))
        {
            let element = match inner {
                Ty::String if nullable => ValueType::NullableString,
                Ty::Class(id) if nullable => ValueType::NullableRecord(id.as_u32()),
                _ => value_type(inner),
            };
            output.extend(std::iter::repeat_n(element, size as usize));
            return Ok(());
        }
        if let Some(inner) = ty.tagged_optional_inner() {
            output.push(ValueType::Bool);
            return self.flatten_with_nullable(inner, true, output);
        }
        if !matches!(
            ty,
            Ty::FixedI64(_)
                | Ty::FixedString(_)
                | Ty::FixedScalar(_, _)
                | Ty::FixedClass(_, _)
                | Ty::FixedStruct(_, _)
                | Ty::FixedEnum(_, _)
                | Ty::Result(_)
                | Ty::Struct(_)
                | Ty::Enum(_)
        ) {
            output.push(match ty {
                Ty::String if nullable => ValueType::NullableString,
                Ty::Class(id) if nullable => ValueType::NullableRecord(id.as_u32()),
                _ => value_type(ty),
            });
            return Ok(());
        }
        let capacity =
            self.width(ty) as usize + self.input.ast.structs.len() + self.input.ast.enums.len();
        #[derive(Clone)]
        enum Work {
            Type(Ty, bool),
            Record(u32),
        }
        let mut pending = self.asm.temporary_vec(capacity)?;
        pending.push(Work::Type(ty, nullable));
        while let Some(work) = pending.pop() {
            let (next, nullable) = match work {
                Work::Type(ty, nullable) => (ty, nullable),
                Work::Record(id) => {
                    output.push(ValueType::NullableRecord(id));
                    continue;
                }
            };
            match next {
                Ty::Result(id) => {
                    output.push(ValueType::I64);
                    let (error, success) = self.checked.result_types[id as usize];
                    if success != Ty::Void {
                        pending.push(Work::Type(success, true));
                    }
                    pending.push(Work::Type(error, true));
                }
                Ty::Struct(id) => {
                    let fields = &self.checked.param_types[self.checked.struct_fields[id]];
                    if fields.is_empty() {
                        output.push(ValueType::I64);
                    } else {
                        pending.extend(fields.iter().rev().map(|&ty| Work::Type(ty, nullable)));
                    }
                }
                Ty::Enum(id) => {
                    output.push(ValueType::I64);
                    let start = self.checked.enum_starts[id] as usize;
                    let count = self.input.ast.enum_cases[self.input.ast.enums[id].cases].len();
                    for index in (0..count).rev() {
                        let case = self.input.ast.enum_cases[self.input.ast.enums[id].cases][index];
                        if case.indirect {
                            pending.push(Work::Record(self.indirect_shape(id, index)));
                        } else {
                            let list = self.checked.enum_case_types[start + index];
                            pending.extend(
                                self.checked.param_types[list]
                                    .iter()
                                    .rev()
                                    .map(|&ty| Work::Type(ty, true)),
                            );
                        }
                    }
                }
                ty if ty.fixed_inner().is_some() => {
                    let (size, inner) = ty.fixed_inner().unwrap();
                    pending.extend(std::iter::repeat_n(
                        Work::Type(inner, nullable),
                        size as usize,
                    ));
                }
                Ty::String if nullable => output.push(ValueType::NullableString),
                Ty::Class(id) if nullable => output.push(ValueType::NullableRecord(id.as_u32())),
                ty if ty.tagged_optional_inner().is_some() => {
                    output.push(ValueType::Bool);
                    pending.push(Work::Type(ty.tagged_optional_inner().unwrap(), true));
                }
                _ => output.push(value_type(next)),
            }
        }
        Ok(())
    }

    fn indirect_shape(&self, id: eitri_syntax::ast::EnumId, case: usize) -> u32 {
        self.input.ast.structs.len() as u32 + self.checked.enum_starts[id] + case as u32
    }

    fn enum_case_offset(&self, id: eitri_syntax::ast::EnumId, case: usize) -> u32 {
        let start = self.checked.enum_starts[id] as usize;
        let cases = &self.input.ast.enum_cases[self.input.ast.enums[id].cases];
        1 + (0..case)
            .map(|index| {
                if cases[index].indirect {
                    1
                } else {
                    let list = self.checked.enum_case_types[start + index];
                    self.checked.param_types[list]
                        .iter()
                        .map(|&ty| self.width(ty))
                        .sum::<u32>()
                }
            })
            .sum::<u32>()
    }

    fn enum_field_offset(&self, id: eitri_syntax::ast::EnumId, case: usize, field: usize) -> u32 {
        let types = self.checked.enum_case_types[self.checked.enum_starts[id] as usize + case];
        self.enum_case_offset(id, case)
            + self.checked.param_types[types][..field]
                .iter()
                .map(|&ty| self.width(ty))
                .sum::<u32>()
    }

    fn move_nullable(
        &mut self,
        ty: Ty,
        source: u32,
        dest: u32,
        wrapping: bool,
        span: Span,
    ) -> Lowered<()> {
        let mut types = self.asm.temporary_vec(self.width(ty) as usize)?;
        self.flatten_type(ty, &mut types)?;
        for (offset, value_ty) in types.into_iter().enumerate() {
            let op = match value_ty {
                ValueType::String => {
                    if wrapping {
                        Op::NullableWrap
                    } else {
                        Op::NullableUnwrap
                    }
                }
                ValueType::Record(_) => {
                    if wrapping {
                        Op::RecordWrap
                    } else {
                        Op::RecordUnwrap
                    }
                }
                _ => Op::Mov,
            };
            self.asm.emit(
                op,
                Operands::abc(
                    (dest + offset as u32).into(),
                    (source + offset as u32).into(),
                    0,
                ),
                span,
            )?;
        }
        Ok(())
    }

    fn field_offset(&self, id: eitri_syntax::ast::StructId, index: u32) -> u32 {
        self.checked.param_types[self.checked.struct_fields[id]][..index as usize]
            .iter()
            .map(|&ty| self.width(ty))
            .sum()
    }

    fn field_address(&self, target: ExprId, binding: BindingId) -> u32 {
        let mut node = target;
        let mut offset = 0;
        while let Expr::Member { base, .. } = self.input.ast.exprs[node] {
            let Res::Field(index) = self.checked.resolutions[node] else {
                unreachable!()
            };
            let Ty::Struct(id) = self.checked.types[base] else {
                unreachable!()
            };
            offset += self.field_offset(id, index);
            node = base;
        }
        if let Res::SelfField(owner_binding, index) = self.checked.resolutions[node] {
            debug_assert_eq!(binding, owner_binding);
            let Ty::Struct(owner) = self.checked.bindings[binding].ty else {
                unreachable!()
            };
            offset += self.field_offset(owner, index);
        }
        self.slots[binding] + offset
    }

    fn dst(&mut self, dst: Option<u32>) -> u32 {
        match dst {
            Some(slot) => slot,
            None => self.temp(),
        }
    }

    // ---- statements ------------------------------------------------------

    /// A block in its own scope. Statements after one that always returns
    /// cannot run, and are not emitted.
    fn statements(&mut self, block: Block) -> Lowered<()> {
        let ast = self.input.ast;
        let mark = self.top;
        for &stmt in ast.block(block) {
            self.stmt(stmt)?;
            if self.stmt_ends(stmt) {
                break;
            }
        }
        self.top = mark;
        Ok(())
    }

    /// Whether `stmt` always returns: `return`, or an `if` statement whose
    /// every branch, including an `else`, does.
    fn stmt_ends(&self, stmt: StmtId) -> bool {
        let ast = self.input.ast;
        match ast.stmts[stmt] {
            Stmt::Return { .. } | Stmt::Break(_) | Stmt::Continue(_) => true,
            Stmt::Expr(e) => match ast.exprs[e] {
                Expr::If(id) => self.checked.types[e] == Ty::Void && self.if_ends(id),
                Expr::Match(id) => {
                    let arms = &ast.match_arms[ast.matches[id].arms];
                    !arms.is_empty()
                        && arms.iter().all(|arm| match arm.body {
                            eitri_syntax::ast::MatchBody::Block(block) => self.ends(block),
                            eitri_syntax::ast::MatchBody::Expr(_) => false,
                        })
                }
                _ => false,
            },
            _ => false,
        }
    }

    fn ends(&self, block: Block) -> bool {
        self.input
            .ast
            .block(block)
            .iter()
            .any(|&stmt| self.stmt_ends(stmt))
    }

    fn if_ends(&self, mut id: IfId) -> bool {
        let ast = self.input.ast;
        loop {
            let IfExpr {
                then, otherwise, ..
            } = ast.ifs[id];
            if !self.ends(then) {
                return false;
            }
            match otherwise {
                Else::None => return false,
                Else::Block(block) => return self.ends(block),
                Else::If(e) => {
                    let Expr::If(next) = ast.exprs[e] else {
                        return false;
                    };
                    id = next;
                }
            }
        }
    }

    fn stmt(&mut self, id: StmtId) -> Lowered<()> {
        let (ast, checked) = (self.input.ast, self.checked);
        let mark = self.top;
        match ast.stmts[id] {
            Stmt::Let { init, .. } => {
                let binding =
                    checked.stmt_bindings[id].expect("a checked `let` declares a binding");
                // The new slot is distinct from any binding it shadows, so
                // `let x = x + 1` reads the old `x` while writing the new.
                let slot = self.temp();
                self.slots[binding] = slot;
                let width = self.width(checked.bindings[binding].ty);
                self.top = slot + width;
                self.expr(init, Some(slot))?;
                self.top = slot + width;
                return Ok(());
            }
            Stmt::Assign {
                op, value, span, ..
            } if checked.stmt_fields[id].is_some()
                && checked.stmt_bindings[id]
                    .is_some_and(|b| matches!(checked.bindings[b].ty, Ty::Class(_))) =>
            {
                let binding = checked.stmt_bindings[id].unwrap();
                let index = checked.stmt_fields[id].unwrap();
                let object = self.slots[binding];
                let Ty::Class(owner) = checked.bindings[binding].ty else {
                    unreachable!()
                };
                let ty = checked.param_types[checked.struct_fields[owner]][index as usize];
                let old = if matches!(op, AssignOp::Compound(_)) {
                    Some(self.class_read_field(object, owner, index, ty, None, span)?)
                } else {
                    None
                };
                let rhs = self.expr(value, None)?;
                let result = if let AssignOp::Compound(binary) = op {
                    let slot = self.temp();
                    self.operate(binary, ty, slot, old.unwrap(), rhs, span)?;
                    slot
                } else {
                    rhs
                };
                self.class_store_field(object, owner, index, ty, result, span)?;
            }
            Stmt::Assign {
                op, value, span, ..
            } => {
                let binding = checked.stmt_bindings[id].expect("a checked assignment has a target");
                let (slot, ty) = if let Some(index) = checked.stmt_fields[id] {
                    let Ty::Struct(owner) = checked.bindings[binding].ty else {
                        unreachable!()
                    };
                    (
                        self.slots[binding] + self.field_offset(owner, index),
                        checked.param_types[checked.struct_fields[owner]][index as usize],
                    )
                } else {
                    (self.slots[binding], checked.bindings[binding].ty)
                };
                match op {
                    AssignOp::Set => {
                        if matches!(
                            ty,
                            Ty::FixedI64(_)
                                | Ty::FixedString(_)
                                | Ty::FixedScalar(_, _)
                                | Ty::FixedClass(_, _)
                                | Ty::FixedStruct(_, _)
                                | Ty::FixedEnum(_, _)
                                | Ty::Result(_)
                                | Ty::Struct(_)
                                | Ty::Enum(_)
                        ) || ty.tagged_optional_inner().is_some()
                        {
                            let source = self.expr(value, None)?;
                            if source != slot {
                                for offset in 0..self.width(ty) {
                                    self.asm.emit(
                                        Op::Mov,
                                        Operands::abc(
                                            (slot + offset).into(),
                                            (source + offset).into(),
                                            0,
                                        ),
                                        span,
                                    )?;
                                }
                            }
                        } else {
                            self.expr(value, Some(slot))?;
                        }
                    }
                    AssignOp::Compound(op) => {
                        // A fault here points at the whole statement.
                        if let (Some(k), Some(imm_op)) =
                            (self.imm8(value).filter(|_| ty == Ty::I64), imm_op(op))
                        {
                            self.asm.emit(
                                imm_op,
                                Operands::abc(slot.into(), slot.into(), k),
                                span,
                            )?;
                        } else {
                            let rhs = self.expr(value, None)?;
                            self.operate(op, ty, slot, slot, rhs, span)?;
                        }
                    }
                }
            }
            Stmt::Expr(e) => self.discard(e)?,
            Stmt::AssignIndex {
                target,
                op,
                value,
                span,
            } => {
                let Expr::Index { base, index } = ast.exprs[target] else {
                    unreachable!()
                };
                let binding = checked.stmt_bindings[id].expect("checked array binding");
                let Some((size, inner)) = checked.types[base].fixed_inner() else {
                    unreachable!()
                };
                let element_width = self.width(inner);
                let class_root = self.class_field_root(base);
                let object = if let Some((class_base, implicit, _, _, _)) = class_root {
                    let slot = self.temp();
                    if let Some(expr) = class_base {
                        self.expr(expr, Some(slot))?;
                    } else {
                        self.asm.emit(
                            Op::Mov,
                            Operands::abc(slot.into(), self.slots[implicit.unwrap()].into(), 0),
                            span,
                        )?;
                    }
                    Some(slot)
                } else {
                    None
                };
                let destination = if class_root.is_none() {
                    self.field_address(base, binding)
                } else {
                    0
                };
                let window = self.top;
                self.top += 1 + u32::from(size);
                let end = self.top;
                self.expr(index, Some(window))?;
                let source = if let Some((_, _, owner, field, root)) = class_root {
                    self.class_read_field(
                        object.unwrap(),
                        owner,
                        field,
                        checked.types[root],
                        None,
                        span,
                    )? + self.class_nested_offset(base, root)
                } else {
                    destination
                };
                let old = self.top;
                self.top += element_width;
                for field in 0..element_width {
                    for item in 0..u32::from(size) {
                        self.asm.emit(
                            Op::Mov,
                            Operands::abc(
                                (window + 1 + item).into(),
                                (source + item * element_width + field).into(),
                                0,
                            ),
                            span,
                        )?;
                    }
                    self.asm.emit(
                        Op::FixedGet,
                        Operands::abc((old + field).into(), window.into(), (size - 1).into()),
                        ast.span(target),
                    )?;
                }
                let rhs = self.expr(value, None)?;
                let result = if let AssignOp::Compound(binary) = op {
                    let slot = self.temp();
                    self.operate(binary, checked.types[target], slot, old, rhs, span)?;
                    slot
                } else {
                    rhs
                };
                let container = if let Some((_, _, owner, field, root)) = class_root {
                    Some(self.class_read_field(
                        object.unwrap(),
                        owner,
                        field,
                        checked.types[root],
                        None,
                        span,
                    )?)
                } else {
                    None
                };
                let refreshed = if let Some((_, _, _, _, root)) = class_root {
                    container.unwrap() + self.class_nested_offset(base, root)
                } else {
                    destination
                };
                for field in 0..element_width {
                    for item in 0..u32::from(size) {
                        self.asm.emit(
                            Op::Mov,
                            Operands::abc(
                                (window + 1 + item).into(),
                                (refreshed + item * element_width + field).into(),
                                0,
                            ),
                            span,
                        )?;
                    }
                    self.asm.emit(
                        Op::FixedSet,
                        Operands::abc((result + field).into(), window.into(), (size - 1).into()),
                        span,
                    )?;
                    for item in 0..u32::from(size) {
                        self.asm.emit(
                            Op::Mov,
                            Operands::abc(
                                (refreshed + item * element_width + field).into(),
                                (window + 1 + item).into(),
                                0,
                            ),
                            span,
                        )?;
                    }
                }
                if let Some((_, _, owner, field, root)) = class_root {
                    self.class_store_field(
                        object.unwrap(),
                        owner,
                        field,
                        checked.types[root],
                        container.unwrap(),
                        span,
                    )?;
                }
                self.top = end;
            }
            Stmt::AssignField {
                target,
                op,
                value,
                span,
            } if self.class_field_root(target).is_some()
                && !matches!(ast.exprs[target], Expr::Member { base, .. } if matches!(checked.types[base], Ty::Class(_))) =>
            {
                let (base, implicit, owner, index, root) = self.class_field_root(target).unwrap();
                let object = if let Some(base) = base {
                    self.expr(base, None)?
                } else {
                    self.slots[implicit.unwrap()]
                };
                let root_ty = checked.types[root];
                let mut cursor = target;
                let mut nested_offset = 0;
                while cursor != root {
                    let Expr::Member { base, .. } = ast.exprs[cursor] else {
                        unreachable!()
                    };
                    let Ty::Struct(struct_id) = checked.types[base] else {
                        unreachable!()
                    };
                    let Res::Field(field) = checked.resolutions[cursor] else {
                        unreachable!()
                    };
                    nested_offset += self.field_offset(struct_id, field);
                    cursor = base;
                }
                let old = if matches!(op, AssignOp::Compound(_)) {
                    Some(
                        self.class_read_field(object, owner, index, root_ty, None, span)?
                            + nested_offset,
                    )
                } else {
                    None
                };
                let rhs = self.expr(value, None)?;
                let result = if let AssignOp::Compound(binary) = op {
                    let slot = self.temp();
                    self.operate(binary, checked.types[target], slot, old.unwrap(), rhs, span)?;
                    slot
                } else {
                    rhs
                };
                let root_slot = self.class_read_field(object, owner, index, root_ty, None, span)?;
                for part in 0..self.width(checked.types[target]) {
                    self.asm.emit(
                        Op::Mov,
                        Operands::abc(
                            (root_slot + nested_offset + part).into(),
                            (result + part).into(),
                            0,
                        ),
                        span,
                    )?;
                }
                self.class_store_field(object, owner, index, root_ty, root_slot, span)?;
            }
            Stmt::AssignField {
                target,
                op,
                value,
                span,
            } if matches!(ast.exprs[target], Expr::Member { base, .. } if matches!(checked.types[base], Ty::Class(_))) =>
            {
                let Expr::Member { base, .. } = ast.exprs[target] else {
                    unreachable!()
                };
                let Res::Field(index) = checked.resolutions[target] else {
                    unreachable!()
                };
                let Ty::Class(owner) = checked.types[base] else {
                    unreachable!()
                };
                let field_ty = checked.types[target];
                let object = self.expr(base, None)?;
                let old = if matches!(op, AssignOp::Compound(_)) {
                    Some(self.class_read_field(object, owner, index, field_ty, None, span)?)
                } else {
                    None
                };
                let rhs = self.expr(value, None)?;
                let result = if let AssignOp::Compound(binary) = op {
                    let slot = self.temp();
                    self.operate(binary, checked.types[target], slot, old.unwrap(), rhs, span)?;
                    slot
                } else {
                    rhs
                };
                self.class_store_field(object, owner, index, field_ty, result, span)?;
            }
            Stmt::AssignField {
                target,
                op,
                value,
                span,
            } => {
                let binding = checked.stmt_bindings[id].expect("checked field target");
                let slot = self.field_address(target, binding);
                match op {
                    AssignOp::Set => {
                        if (matches!(
                            checked.types[target],
                            Ty::FixedI64(_)
                                | Ty::FixedString(_)
                                | Ty::FixedScalar(_, _)
                                | Ty::FixedClass(_, _)
                                | Ty::FixedStruct(_, _)
                                | Ty::FixedEnum(_, _)
                                | Ty::Result(_)
                                | Ty::Struct(_)
                                | Ty::Enum(_)
                        ) || checked.types[target].tagged_optional_inner().is_some())
                        {
                            let source = self.expr(value, None)?;
                            if source != slot {
                                let width = self.width(checked.types[target]);
                                // A nested source and target may overlap; evaluate into a distinct window.
                                let temporary = self.top;
                                self.top += width;
                                for offset in 0..width {
                                    self.asm.emit(
                                        Op::Mov,
                                        Operands::abc(
                                            (temporary + offset).into(),
                                            (source + offset).into(),
                                            0,
                                        ),
                                        span,
                                    )?;
                                }
                                for offset in 0..width {
                                    self.asm.emit(
                                        Op::Mov,
                                        Operands::abc(
                                            (slot + offset).into(),
                                            (temporary + offset).into(),
                                            0,
                                        ),
                                        span,
                                    )?;
                                }
                            }
                        } else {
                            self.expr(value, Some(slot))?;
                        }
                    }
                    AssignOp::Compound(op) => {
                        let rhs = self.expr(value, None)?;
                        self.operate(op, checked.types[target], slot, slot, rhs, span)?;
                    }
                }
            }
            Stmt::Break(keyword) | Stmt::Continue(keyword) => {
                let &(continue_to, break_to) = self.loops.last().expect("checked loop control");
                let target = if matches!(ast.stmts[id], Stmt::Break(_)) {
                    break_to
                } else {
                    continue_to
                };
                self.asm.jump(target, self.input.tokens.span(keyword));
            }
            Stmt::For(for_id) => {
                let eitri_syntax::ast::ForStmt {
                    keyword,
                    start,
                    end,
                    inclusive,
                    body,
                    ..
                } = ast.fors[for_id];
                let binding = checked.stmt_bindings[id].expect("checked for binding");
                if end.is_none() {
                    let base =
                        if matches!(checked.resolutions[start], Res::ForChars | Res::ForLines) {
                            let Expr::Call { callee, .. } = ast.exprs[start] else {
                                unreachable!()
                            };
                            let Expr::Member { base, .. } = ast.exprs[callee] else {
                                unreachable!()
                            };
                            base
                        } else {
                            start
                        };
                    let string = self.temp();
                    self.expr(base, Some(string))?;
                    self.top = string + 1;
                    let length = self.temp();
                    self.asm.emit(
                        Op::StrLen,
                        Operands::abc(length.into(), string.into(), 0),
                        ast.span(start),
                    )?;
                    let current = self.temp();
                    self.asm.load(current, 0, ValueType::I64, ast.span(start))?;
                    let first_slot = self.temp();
                    self.slots[binding] = first_slot;
                    let result = if ast.fors[for_id].second.is_some() {
                        let second = BindingId::from_u32(binding.as_u32() + 1);
                        let slot = self.temp();
                        self.slots[second] = slot;
                        slot
                    } else {
                        first_slot
                    };
                    let top = self.asm.label();
                    let exit = self.asm.label();
                    let span = self.input.tokens.span(keyword);
                    self.asm.bind(top)?;
                    self.asm.branch_if(Cond::Le, length, current, exit, span)?;
                    if ast.fors[for_id].second.is_some() {
                        self.asm.emit(
                            Op::Mov,
                            Operands::abc(first_slot.into(), current.into(), 0),
                            span,
                        )?;
                    }
                    let op = if checked.resolutions[start] == Res::ForLines {
                        Op::StrNextLine
                    } else {
                        Op::StrNextChar
                    };
                    self.asm.emit(
                        op,
                        Operands::abc(result.into(), string.into(), current.into()),
                        span,
                    )?;
                    let next = self.asm.label();
                    self.loops.push((next, exit));
                    let body_result = self.statements(body);
                    self.loops.pop();
                    body_result?;
                    self.asm.bind(next)?;
                    self.asm.jump(top, span);
                    self.asm.bind(exit)?;
                    self.top = mark;
                    return Ok(());
                }
                let current = self.temp();
                self.expr(start, Some(current))?;
                self.top = current + 1;
                let limit = self.temp();
                self.expr(end.expect("checked range"), Some(limit))?;
                self.top = limit + 1;
                self.slots[binding] = current;
                let top = self.asm.label();
                let exit = self.asm.label();
                let span = self.input.tokens.span(keyword);
                let inclusive_exit = if inclusive {
                    let poll = self.asm.label();
                    self.asm.jump(top, span);
                    self.asm.bind(poll)?;
                    self.asm.jump(exit, span);
                    Some(poll)
                } else {
                    None
                };
                self.asm.bind(top)?;
                let ty = checked.bindings[binding].ty;
                let cond = match (ty.signed(), inclusive) {
                    (true, true) => Cond::Lt,
                    (true, false) => Cond::Le,
                    (false, true) => Cond::Lo,
                    (false, false) => Cond::Ls,
                };
                self.asm.branch_if(cond, limit, current, exit, span)?;
                let next = self.asm.label();
                self.loops.push((next, exit));
                let body_result = self.statements(body);
                self.loops.pop();
                body_result?;
                self.asm.bind(next)?;
                if inclusive {
                    self.asm.branch_if(
                        Cond::Eq,
                        current,
                        limit,
                        inclusive_exit.expect("inclusive exit"),
                        span,
                    )?;
                }
                let one = self.temp();
                self.asm.load(one, 1, value_type(ty), span)?;
                self.operate(BinaryOp::Add, ty, current, current, one, span)?;
                self.asm.jump(top, span);
                self.asm.bind(exit)?;
            }
            Stmt::While {
                keyword,
                condition,
                body,
            } => {
                let top = self.asm.label();
                let exit = self.asm.label();
                self.asm.bind(top)?;
                self.branch(condition, false, exit)?;
                self.loops.push((top, exit));
                let body_result = self.statements(body);
                self.loops.pop();
                body_result?;
                // The back edge polls the budget; a fault there points at
                // `while`.
                self.asm.jump(top, self.input.tokens.span(keyword));
                self.asm.bind(exit)?;
            }
            Stmt::Return { keyword, value } => {
                if let Some(value) = value {
                    self.expr(value, Some(0))?;
                } else if let Some(Ty::Result(id)) = Some(self.current_ret) {
                    self.result_value(
                        None,
                        id,
                        true,
                        None,
                        Some(0),
                        self.input.tokens.span(keyword),
                    )?;
                }
                self.write_mutation_result(self.input.tokens.span(keyword))?;
                self.asm.ret(self.input.tokens.span(keyword))?;
            }
        }
        self.top = mark;
        Ok(())
    }

    /// An expression statement: its value, if any, is dropped.
    fn discard(&mut self, e: ExprId) -> Lowered<()> {
        if let Expr::If(id) = self.input.ast.exprs[e]
            && self.checked.types[e] == Ty::Void
        {
            return self.if_stmt(id);
        }
        self.expr(e, None).map(drop)
    }

    /// An `if` statement, iterating along its `else if` chain.
    fn if_stmt(&mut self, id: IfId) -> Lowered<()> {
        let ast = self.input.ast;
        let end = self.asm.label();
        let mut current = id;
        loop {
            let IfExpr {
                condition,
                then,
                otherwise,
            } = ast.ifs[current];
            let next = self.asm.label();
            self.branch(condition, false, next)?;
            self.statements(then)?;
            if !matches!(otherwise, Else::None) && !self.ends(then) {
                self.asm.jump(end, then.span);
            }
            self.asm.bind(next)?;
            match otherwise {
                Else::None => break,
                Else::Block(block) => {
                    self.statements(block)?;
                    break;
                }
                Else::If(e) => {
                    let Expr::If(nested) = ast.exprs[e] else {
                        unreachable!("`else if` holds an `if`");
                    };
                    current = nested;
                }
            }
        }
        self.asm.bind(end)
    }

    // ---- conditions ------------------------------------------------------

    /// Jump to `target` when `e` evaluates to `when`; fall through otherwise.
    fn branch(&mut self, e: ExprId, when: bool, target: Label) -> Lowered<()> {
        let ast = self.input.ast;
        let span = ast.span(e);
        let mark = self.top;
        match ast.exprs[e] {
            Expr::Binary { op, lhs, rhs }
                if !SIMPLE
                    && compare(op).is_some()
                    && matches!(self.checked.types[lhs], Ty::I64 | Ty::Bool)
                    && self.checked.types[lhs] == self.checked.types[rhs]
                    && !self.checked.optional_wrap[lhs]
                    && !self.checked.optional_wrap[rhs] =>
            {
                // Against a small literal: one `CBI`, no load (E5, C2).
                if let Some((x, cond, k)) = self.compare_imm(op, lhs, rhs) {
                    let x = self.expr(x, None)?;
                    let (cond, k) = if when {
                        (cond, k)
                    } else {
                        imm_complement(cond, k).expect("checked by compare_imm")
                    };
                    self.asm.branch_imm(cond, x, k, target, span)?;
                    self.top = mark;
                    return Ok(());
                }
                let a = self.expr(lhs, None)?;
                let b = self.expr(rhs, None)?;
                let (cond, swap) = compare(op).expect("matched above");
                let (a, b) = if swap { (b, a) } else { (a, b) };
                let (cond, a, b) = if when {
                    (cond, a, b)
                } else {
                    let (complement, swap) = cond
                        .complement()
                        .expect("register-form conditions have complements");
                    if swap {
                        (complement, b, a)
                    } else {
                        (complement, a, b)
                    }
                };
                self.asm.branch_if(cond, a, b, target, span)?;
            }
            Expr::Binary {
                op: BinaryOp::And | BinaryOp::Or,
                ..
            } => self.logic_branch(e, when, target)?,
            Expr::Unary {
                op: UnaryOp::Not,
                operand,
            } => self.branch(operand, !when, target)?,
            Expr::Bool(value) => {
                if value == when {
                    self.asm.jump(target, span);
                }
            }
            _ => {
                let value = self.expr(e, None)?;
                let test = if when { Test::True } else { Test::False };
                self.asm.branch_test(test, value, target, span);
            }
        }
        self.top = mark;
        Ok(())
    }

    /// `a && b && …` or `a || b || …`, short-circuiting, over the chain's
    /// operands in source order without recursion.
    fn logic_branch(&mut self, e: ExprId, when: bool, target: Label) -> Lowered<()> {
        let Expr::Binary { op, .. } = self.input.ast.exprs[e] else {
            unreachable!("called on `&&` or `||`");
        };
        let mark = self.stack.len();
        self.operands(e, op);
        let count = self.stack.len() - mark;
        // The value that decides the chain early: false for `&&`, true for `||`.
        let decides = op == BinaryOp::Or;
        let skip = self.asm.label();
        for index in 0..count {
            let operand = self.stack[mark + index];
            if when == decides || index + 1 == count {
                self.branch(operand, when, target)?;
            } else {
                self.branch(operand, decides, skip)?;
            }
        }
        self.asm.bind(skip)?;
        self.stack.truncate(mark);
        Ok(())
    }

    /// Push the operands of a homogeneous `op` chain rooted at `root`, in
    /// source order.
    fn operands(&mut self, root: ExprId, op: BinaryOp) {
        let ast = self.input.ast;
        let mark = self.stack.len();
        let mut node = root;
        while let Expr::Binary { op: o, lhs, rhs } = ast.exprs[node]
            && o == op
        {
            self.stack.push(rhs);
            node = lhs;
        }
        self.stack.push(node);
        self.stack[mark..].reverse();
    }

    // ---- values ----------------------------------------------------------

    /// Lower `e` into `dst` when given, or anywhere convenient; return the
    /// slot holding its value.
    fn expr(&mut self, e: ExprId, dst: Option<u32>) -> Lowered<u32> {
        if let Some(id) = self.checked.result_wrap[e] {
            return self.result_value(Some(e), id, true, Some(e), dst, self.input.ast.span(e));
        }
        if self.checked.optional_wrap[e] && self.checked.types[e] == Ty::FindOffset {
            let target = if let Some(dst) = dst {
                dst
            } else {
                let slot = self.top;
                self.top += 2;
                slot
            };
            let source = self.expr_inner(e, None)?;
            let done = self.asm.label();
            self.asm
                .load(target, 0, ValueType::Bool, self.input.ast.span(e))?;
            self.asm
                .load(target + 1, 0, ValueType::I64, self.input.ast.span(e))?;
            self.asm
                .branch_imm(Cond::Eq, source, -1, done, self.input.ast.span(e))?;
            self.asm
                .load(target, 1, ValueType::Bool, self.input.ast.span(e))?;
            self.asm.emit(
                Op::Mov,
                Operands::abc((target + 1).into(), source.into(), 0),
                self.input.ast.span(e),
            )?;
            self.asm.bind(done)?;
            return Ok(target);
        }
        if self.checked.optional_wrap[e]
            && !matches!(self.checked.types[e], Ty::String | Ty::Class(_))
        {
            let inner = self.checked.types[e];
            let target = if let Some(dst) = dst {
                dst
            } else {
                let slot = self.top;
                self.top += 1 + self.width(inner);
                slot
            };
            if matches!(
                inner,
                Ty::FixedI64(_)
                    | Ty::FixedString(_)
                    | Ty::FixedScalar(_, _)
                    | Ty::FixedClass(_, _)
                    | Ty::FixedStruct(_, _)
                    | Ty::FixedEnum(_, _)
                    | Ty::Result(_)
                    | Ty::Struct(_)
                    | Ty::Enum(_)
            ) {
                let source = self.expr_inner(e, None)?;
                self.move_nullable(inner, source, target + 1, true, self.input.ast.span(e))?;
            } else {
                self.expr_inner(e, Some(target + 1))?;
            }
            self.asm
                .load(target, 1, ValueType::Bool, self.input.ast.span(e))?;
            return Ok(target);
        }
        if self.checked.optional_wrap[e] {
            let target = self.dst(dst);
            let source = self.expr_inner(e, None)?;
            self.asm.emit(
                if matches!(self.checked.types[e], Ty::Class(_)) {
                    Op::RecordWrap
                } else {
                    Op::NullableWrap
                },
                Operands::abc(target.into(), source.into(), 0),
                self.input.ast.span(e),
            )?;
            return Ok(target);
        }
        self.expr_inner(e, dst)
    }

    fn result_value(
        &mut self,
        expr: Option<ExprId>,
        id: u32,
        is_ok: bool,
        payload: Option<ExprId>,
        dst: Option<u32>,
        span: Span,
    ) -> Lowered<u32> {
        let ty = Ty::Result(id);
        let width = self.width(ty);
        let target = if let Some(dst) = dst {
            dst
        } else {
            let target = self.top;
            self.top += width;
            target
        };
        let mut layout = self.asm.temporary_vec(width as usize)?;
        self.flatten_type(ty, &mut layout)?;
        for (offset, ty) in layout.into_iter().enumerate() {
            let slot = target + offset as u32;
            match ty {
                ValueType::NullableRecord(id) => self.asm.emit(
                    Op::RecordNull,
                    Operands::abc(slot.into(), 0, id.into()),
                    span,
                )?,
                ValueType::NullableString => {
                    self.asm
                        .emit(Op::NullableNull, Operands::abc(slot.into(), 0, 0), span)?
                }
                ty => self.asm.load(slot, 0, ty, span)?,
            }
        }
        self.asm
            .load(target, u64::from(is_ok), ValueType::I64, span)?;
        if let Some(payload) = payload {
            let (error, success) = self.checked.result_types[id as usize];
            let payload_ty = if is_ok { success } else { error };
            let source = if Some(payload) == expr {
                self.expr_inner(payload, None)?
            } else {
                self.expr(payload, None)?
            };
            let dest = target + 1 + if is_ok { self.width(error) } else { 0 };
            self.move_nullable(payload_ty, source, dest, true, span)?;
        }
        Ok(target)
    }

    fn expr_inner(&mut self, e: ExprId, dst: Option<u32>) -> Lowered<u32> {
        let (ast, checked) = (self.input.ast, self.checked);
        let span = ast.span(e);
        if let Res::ResultCase(id, is_ok) = checked.resolutions[e] {
            let payload = match ast.exprs[e] {
                Expr::Call { args, .. } => ast.args(args).first().copied(),
                Expr::NamedCall { args, .. } => ast.field_inits[args].first().map(|arg| arg.value),
                _ => None,
            };
            return self.result_value(Some(e), id, is_ok, payload, dst, span);
        }
        if let Res::EnumCase(id, case) = checked.resolutions[e] {
            let width = self.width(Ty::Enum(id));
            let slot = match dst {
                Some(slot) => slot,
                None => {
                    let slot = self.top;
                    self.top += width;
                    slot
                }
            };
            let end = self.top;
            self.asm.load(slot, case as u64, ValueType::I64, span)?;
            let cases = &ast.enum_cases[ast.enums[id].cases];
            let mut layout = self.asm.temporary_vec(width as usize)?;
            self.flatten_type(Ty::Enum(id), &mut layout)?;
            for (offset, &ty) in layout.iter().enumerate().skip(1) {
                if let ValueType::NullableRecord(id) = ty {
                    self.asm.emit(
                        Op::RecordNull,
                        Operands::abc((slot + offset as u32).into(), 0, id.into()),
                        span,
                    )?;
                } else if ty == ValueType::NullableString {
                    self.asm.emit(
                        Op::NullableNull,
                        Operands::abc((slot + offset as u32).into(), 0, 0),
                        span,
                    )?;
                } else {
                    self.asm.load(slot + offset as u32, 0, ty, span)?;
                }
            }
            if cases[case as usize].indirect {
                let fields = ast.params(cases[case as usize].fields);
                let types =
                    checked.enum_case_types[checked.enum_starts[id] as usize + case as usize];
                let payload_width: u32 = checked.param_types[types]
                    .iter()
                    .map(|&ty| self.width(ty))
                    .sum();
                let first = self.top;
                self.top += payload_width.max(1);
                let temporary = self.top;
                if let Expr::NamedCall { args, .. } = ast.exprs[e] {
                    for (position, arg) in ast.field_inits[args].iter().enumerate() {
                        let index = if self.input.tokens.kind(arg.name)
                            == eitri_syntax::TokenKind::LParen
                        {
                            position
                        } else {
                            fields
                                .iter()
                                .position(|field| {
                                    self.input.tokens.symbol(field.name)
                                        == self.input.tokens.symbol(arg.name)
                                })
                                .expect("checked field")
                        };
                        let offset: u32 = checked.param_types[types][..index]
                            .iter()
                            .map(|&ty| self.width(ty))
                            .sum();
                        self.expr(arg.value, Some(first + offset))?;
                        self.top = temporary;
                    }
                }
                if let Expr::Call { args, .. } = ast.exprs[e] {
                    for (index, &arg) in ast.args(args).iter().enumerate() {
                        let offset: u32 = checked.param_types[types][..index]
                            .iter()
                            .map(|&ty| self.width(ty))
                            .sum();
                        self.expr(arg, Some(first + offset))?;
                        self.top = temporary;
                    }
                }
                let record = self.temp();
                self.asm.emit(
                    Op::RecordNew,
                    Operands::abc(
                        record.into(),
                        first.into(),
                        self.indirect_shape(id, case as usize).into(),
                    ),
                    span,
                )?;
                self.asm.emit(
                    Op::RecordWrap,
                    Operands::abc(
                        (slot + self.enum_case_offset(id, case as usize)).into(),
                        record.into(),
                        0,
                    ),
                    span,
                )?;
                self.top = end;
                return Ok(slot);
            }
            if let Expr::NamedCall { args, .. } = ast.exprs[e] {
                let fields = ast.params(cases[case as usize].fields);
                for (position, arg) in ast.field_inits[args].iter().enumerate() {
                    let index =
                        if self.input.tokens.kind(arg.name) == eitri_syntax::TokenKind::LParen {
                            position
                        } else {
                            fields
                                .iter()
                                .position(|f| {
                                    self.input.tokens.symbol(f.name)
                                        == self.input.tokens.symbol(arg.name)
                                })
                                .expect("checked payload")
                        };
                    let types =
                        checked.enum_case_types[checked.enum_starts[id] as usize + case as usize];
                    let ty = checked.param_types[types][index];
                    let value = self.expr(arg.value, None)?;
                    self.move_nullable(
                        ty,
                        value,
                        slot + self.enum_field_offset(id, case as usize, index),
                        true,
                        span,
                    )?;
                    self.top = end;
                }
            }
            if let Expr::Call { args, .. } = ast.exprs[e] {
                let types =
                    checked.enum_case_types[checked.enum_starts[id] as usize + case as usize];
                for (index, &arg) in ast.args(args).iter().enumerate() {
                    let ty = checked.param_types[types][index];
                    let value = self.expr(arg, None)?;
                    self.move_nullable(
                        ty,
                        value,
                        slot + self.enum_field_offset(id, case as usize, index),
                        true,
                        span,
                    )?;
                    self.top = end;
                }
            }
            return Ok(slot);
        }
        match ast.exprs[e] {
            Expr::Try(operand) => {
                let Ty::Result(source_id) = checked.types[operand] else {
                    unreachable!("checked try result")
                };
                let Res::TryResult(target_id) = checked.resolutions[e] else {
                    unreachable!("checked try target")
                };
                let (error, success) = checked.result_types[source_id as usize];
                let target = if success == Ty::Void {
                    0
                } else if let Some(dst) = dst {
                    dst
                } else {
                    let slot = self.top;
                    self.top += self.width(success);
                    slot
                };
                let source = self.expr(operand, None)?;
                let ok = self.asm.label();
                self.asm.branch_imm(Cond::Ne, source, 0, ok, span)?;
                self.result_value(None, target_id, false, None, Some(0), span)?;
                let temp = self.top;
                self.top += self.width(error);
                self.move_nullable(error, source + 1, temp, false, span)?;
                self.move_nullable(error, temp, 1, true, span)?;
                self.write_mutation_result(span)?;
                self.asm.ret(span)?;
                self.asm.bind(ok)?;
                if success != Ty::Void {
                    self.move_nullable(
                        success,
                        source + 1 + self.width(error),
                        target,
                        false,
                        span,
                    )?;
                }
                Ok(target)
            }
            Expr::Index { base, index } => {
                let (size, inner) = checked.types[base].fixed_inner().unwrap();
                let element_width = self.width(inner);
                let target = if let Some(dst) = dst {
                    dst
                } else {
                    let slot = self.top;
                    self.top += element_width;
                    slot
                };
                let source = self.top;
                self.top += u32::from(size) * element_width;
                let window = self.top;
                self.top += 1 + u32::from(size);
                let end = self.top;
                self.expr(base, Some(source))?;
                self.top = end;
                self.expr(index, Some(window))?;
                for field in 0..element_width {
                    for item in 0..u32::from(size) {
                        self.asm.emit(
                            Op::Mov,
                            Operands::abc(
                                (window + 1 + item).into(),
                                (source + item * element_width + field).into(),
                                0,
                            ),
                            span,
                        )?;
                    }
                    self.asm.emit(
                        Op::FixedGet,
                        Operands::abc((target + field).into(), window.into(), (size - 1).into()),
                        span,
                    )?;
                }
                Ok(target)
            }
            Expr::FixedArray(elements) => {
                let width = self.width(checked.types[e]);
                let slot = if let Some(dst) = dst {
                    dst
                } else {
                    let slot = self.top;
                    self.top += width;
                    slot
                };
                let end = self.top;
                for (index, &element) in ast.args(elements).iter().enumerate() {
                    self.expr(
                        element,
                        Some(slot + index as u32 * self.width(checked.types[element])),
                    )?;
                    self.top = end;
                }
                Ok(slot)
            }
            Expr::Int { .. } | Expr::Float { .. } | Expr::Char(_) => {
                let slot = self.dst(dst);
                let value = checked.int_value(e).expect("checked literals have values");
                self.asm
                    .load(slot, value as u64, value_type(checked.types[e]), span)?;
                Ok(slot)
            }
            Expr::String(_) | Expr::StringPart(_) => {
                let Res::StringLiteral(index) = checked.resolutions[e] else {
                    unreachable!("checked string literal")
                };
                let slot = self.dst(dst);
                self.asm.emit(
                    Op::StrNew,
                    Operands::abc(slot.into(), index.into(), 0),
                    span,
                )?;
                Ok(slot)
            }
            Expr::Interpolation(parts) => self.interpolation(e, parts, dst),
            Expr::Null(_) => {
                if let Ty::OptionalClass(id) = checked.types[e] {
                    let slot = self.dst(dst);
                    self.asm.emit(
                        Op::RecordNull,
                        Operands::abc(slot.into(), 0, id.as_u32().into()),
                        span,
                    )?;
                    Ok(slot)
                } else if checked.types[e].tagged_optional_inner().is_some() {
                    let slot = if let Some(dst) = dst {
                        dst
                    } else {
                        let slot = self.top;
                        self.top += self.width(checked.types[e]);
                        slot
                    };
                    let mut layout = self
                        .asm
                        .temporary_vec(self.width(checked.types[e]) as usize)?;
                    self.flatten_type(checked.types[e], &mut layout)?;
                    for (offset, ty) in layout.into_iter().enumerate() {
                        if let ValueType::NullableRecord(id) = ty {
                            self.asm.emit(
                                Op::RecordNull,
                                Operands::abc((slot + offset as u32).into(), 0, id.into()),
                                span,
                            )?;
                        } else if ty == ValueType::NullableString {
                            self.asm.emit(
                                Op::NullableNull,
                                Operands::abc((slot + offset as u32).into(), 0, 0),
                                span,
                            )?;
                        } else {
                            self.asm.load(slot + offset as u32, 0, ty, span)?;
                        }
                    }
                    Ok(slot)
                } else {
                    let slot = self.dst(dst);
                    self.asm
                        .emit(Op::NullableNull, Operands::abc(slot.into(), 0, 0), span)?;
                    Ok(slot)
                }
            }
            Expr::Bool(value) => {
                let slot = self.dst(dst);
                self.asm
                    .load(slot, u64::from(value), ValueType::Bool, span)?;
                Ok(slot)
            }
            Expr::StructLiteral { fields, .. } if matches!(checked.types[e], Ty::Class(_)) => {
                let Ty::Class(id) = checked.types[e] else {
                    unreachable!()
                };
                self.class_literal(id, fields, dst, span)
            }
            Expr::StructLiteral { fields, .. } => {
                let Res::Struct(id) = checked.resolutions[e] else {
                    unreachable!()
                };
                let width = self.width(Ty::Struct(id));
                let slot = if let Some(dst) = dst {
                    dst
                } else {
                    let first = self.top;
                    self.top += width;
                    first
                };
                let end = self.top;
                let declared = ast.params(ast.structs[id].fields);
                let entries = &ast.field_inits[fields];
                let base = entries
                    .last()
                    .filter(|field| {
                        self.input.tokens.kind(field.name) == eitri_syntax::TokenKind::DotDot
                    })
                    .map(|field| field.value);
                if declared.is_empty() {
                    self.asm.load(slot, 0, ValueType::I64, span)?;
                }
                for field in entries.iter().filter(|field| {
                    self.input.tokens.kind(field.name) != eitri_syntax::TokenKind::DotDot
                }) {
                    let symbol = self.input.tokens.symbol(field.name);
                    let index = declared
                        .iter()
                        .position(|item| self.input.tokens.symbol(item.name) == symbol)
                        .expect("checked field");
                    self.expr(
                        field.value,
                        Some(slot + self.field_offset(id, index as u32)),
                    )?;
                    self.top = end;
                }
                if let Some(base) = base {
                    let copy = self.top;
                    self.top += width;
                    self.expr(base, Some(copy))?;
                    for (index, field) in declared.iter().enumerate() {
                        if !entries
                            .iter()
                            .filter(|init| {
                                self.input.tokens.kind(init.name) != eitri_syntax::TokenKind::DotDot
                            })
                            .any(|init| {
                                self.input.tokens.symbol(init.name)
                                    == self.input.tokens.symbol(field.name)
                            })
                        {
                            let offset = self.field_offset(id, index as u32);
                            let ty = checked.param_types[checked.struct_fields[id]][index];
                            for part in 0..self.width(ty) {
                                self.asm.emit(
                                    Op::Mov,
                                    Operands::abc(
                                        (slot + offset + part).into(),
                                        (copy + offset + part).into(),
                                        0,
                                    ),
                                    span,
                                )?;
                            }
                        }
                    }
                    self.top = end;
                }
                for (index, field) in declared.iter().enumerate() {
                    if base.is_none()
                        && let Some(default) = field.default
                        && !entries
                            .iter()
                            .filter(|init| {
                                self.input.tokens.kind(init.name) != eitri_syntax::TokenKind::DotDot
                            })
                            .any(|init| {
                                self.input.tokens.symbol(init.name)
                                    == self.input.tokens.symbol(field.name)
                            })
                    {
                        if self.default_depth >= 16 || self.default_expansions >= 65_536 {
                            return Err(AsmError::CodeTooLarge);
                        }
                        self.default_depth += 1;
                        self.default_expansions += 1;
                        let result =
                            self.expr(default, Some(slot + self.field_offset(id, index as u32)));
                        self.default_depth -= 1;
                        result?;
                        self.top = end;
                    }
                }
                Ok(slot)
            }
            Expr::Member { base, .. }
                if matches!(checked.types[base], Ty::Class(_))
                    && matches!(checked.resolutions[e], Res::Field(_)) =>
            {
                let Res::Field(index) = checked.resolutions[e] else {
                    unreachable!()
                };
                let Ty::Class(owner) = checked.types[base] else {
                    unreachable!()
                };
                let source = self.expr(base, None)?;
                self.class_read_field(source, owner, index, checked.types[e], dst, span)
            }
            Expr::Member { base, .. } if matches!(checked.resolutions[e], Res::Field(_)) => {
                let Res::Field(index) = checked.resolutions[e] else {
                    unreachable!()
                };
                let Ty::Struct(id) = checked.types[base] else {
                    unreachable!()
                };
                let source = self.expr(base, None)? + self.field_offset(id, index);
                if (matches!(
                    checked.types[e],
                    Ty::FixedI64(_)
                        | Ty::FixedString(_)
                        | Ty::FixedScalar(_, _)
                        | Ty::FixedClass(_, _)
                        | Ty::FixedStruct(_, _)
                        | Ty::FixedEnum(_, _)
                        | Ty::Result(_)
                        | Ty::Struct(_)
                        | Ty::Enum(_)
                ) || checked.types[e].tagged_optional_inner().is_some())
                {
                    let width = self.width(checked.types[e]);
                    let target = match dst {
                        Some(d) => d,
                        None if SIMPLE => {
                            let d = self.top;
                            self.top += width;
                            d
                        }
                        None => source,
                    };
                    if target != source {
                        for offset in 0..width {
                            self.asm.emit(
                                Op::Mov,
                                Operands::abc(
                                    (target + offset).into(),
                                    (source + offset).into(),
                                    0,
                                ),
                                span,
                            )?;
                        }
                    }
                    Ok(target)
                } else {
                    match dst {
                        Some(target) if target != source => {
                            self.asm.emit(
                                Op::Mov,
                                Operands::abc(target.into(), source.into(), 0),
                                span,
                            )?;
                            Ok(target)
                        }
                        _ => Ok(source),
                    }
                }
            }
            Expr::SelfValue(_) | Expr::Name(_) if matches!(checked.resolutions[e], Res::SelfField(binding, _) if matches!(checked.bindings[binding].ty, Ty::Class(_))) =>
            {
                let Res::SelfField(binding, index) = checked.resolutions[e] else {
                    unreachable!()
                };
                let Ty::Class(owner) = checked.bindings[binding].ty else {
                    unreachable!()
                };
                self.class_read_field(
                    self.slots[binding],
                    owner,
                    index,
                    checked.types[e],
                    dst,
                    span,
                )
            }
            Expr::SelfValue(_) | Expr::Name(_) => {
                let slot = match checked.resolutions[e] {
                    Res::Binding(binding) => self.slots[binding],
                    Res::SelfField(binding, index) => {
                        let Ty::Struct(id) = checked.bindings[binding].ty else {
                            unreachable!()
                        };
                        self.slots[binding] + self.field_offset(id, index)
                    }
                    _ => unreachable!("checked value names resolve to bindings or fields"),
                };
                if (matches!(
                    checked.types[e],
                    Ty::FixedI64(_)
                        | Ty::FixedString(_)
                        | Ty::FixedScalar(_, _)
                        | Ty::FixedClass(_, _)
                        | Ty::FixedStruct(_, _)
                        | Ty::FixedEnum(_, _)
                        | Ty::Result(_)
                        | Ty::Struct(_)
                        | Ty::Enum(_)
                ) || checked.types[e].tagged_optional_inner().is_some())
                {
                    let width = self.width(checked.types[e]);
                    let target = match dst {
                        Some(d) => d,
                        None if SIMPLE => {
                            let first = self.top;
                            self.top += width;
                            first
                        }
                        None => slot,
                    };
                    if target != slot {
                        for offset in 0..width {
                            self.asm.emit(
                                Op::Mov,
                                Operands::abc((target + offset).into(), (slot + offset).into(), 0),
                                span,
                            )?;
                        }
                    }
                    return Ok(target);
                }
                match dst {
                    Some(d) if d != slot => {
                        self.asm
                            .emit(Op::Mov, Operands::abc(d.into(), slot.into(), 0), span)?;
                        Ok(d)
                    }
                    None if SIMPLE => {
                        let copy = self.temp();
                        self.asm
                            .emit(Op::Mov, Operands::abc(copy.into(), slot.into(), 0), span)?;
                        Ok(copy)
                    }
                    _ => Ok(slot),
                }
            }
            Expr::Unary {
                op: UnaryOp::Neg | UnaryOp::BitNot,
                operand,
            } => {
                let x = self.expr(operand, None)?;
                let slot = self.dst(dst);
                let ty = checked.types[e];
                let Expr::Unary { op, .. } = ast.exprs[e] else {
                    unreachable!()
                };
                let opcode = if op == UnaryOp::BitNot {
                    Op::Not
                } else if ty == Ty::F32 {
                    Op::FnegF32
                } else if ty == Ty::F64 {
                    Op::FnegF64
                } else if !ty.signed() {
                    Op::NegChkU
                } else if ty == Ty::I8 {
                    Op::NegWrap
                } else {
                    Op::NegChkS
                };
                self.asm
                    .emit(opcode, Operands::abc(slot.into(), x.into(), 0), span)?;
                if ty == Ty::I8 || ty == Ty::U8 {
                    let from = if ty.signed() { Ty::I64 } else { Ty::U64 };
                    self.convert(slot, slot, from, ty, op != UnaryOp::BitNot, span)?;
                }
                Ok(slot)
            }
            Expr::Unary {
                op: UnaryOp::Not,
                operand,
            } => {
                // `!b` is `b ^ true`; the 1 is typed `bool` for the verifier.
                let x = self.expr(operand, None)?;
                let one = self.temp();
                self.asm.load(one, 1, ValueType::Bool, span)?;
                let slot = self.dst(dst);
                self.asm.emit(
                    Op::Xor,
                    Operands::abc(slot.into(), x.into(), one.into()),
                    span,
                )?;
                Ok(slot)
            }
            Expr::Binary {
                op: BinaryOp::And | BinaryOp::Or,
                ..
            } => self.logic_value(e, dst),
            Expr::Binary {
                op: BinaryOp::Add, ..
            } if checked.types[e] == Ty::String => self.string_join(e, dst),
            Expr::Binary {
                op: BinaryOp::Eq | BinaryOp::NotEq,
                lhs,
                rhs,
            } if checked.types[lhs] == Ty::OptionalString
                || (checked.optional_wrap[lhs] && checked.types[lhs] == Ty::String) =>
            {
                let left = self.expr(lhs, None)?;
                let right = self.expr(rhs, None)?;
                let slot = self.dst(dst);
                self.asm.emit(
                    Op::NullableEq,
                    Operands::abc(slot.into(), left.into(), right.into()),
                    span,
                )?;
                if matches!(
                    ast.exprs[e],
                    Expr::Binary {
                        op: BinaryOp::NotEq,
                        ..
                    }
                ) {
                    let one = self.temp();
                    self.asm.load(one, 1, ValueType::Bool, span)?;
                    self.asm.emit(
                        Op::Xor,
                        Operands::abc(slot.into(), slot.into(), one.into()),
                        span,
                    )?;
                }
                Ok(slot)
            }
            Expr::Binary { op, lhs, rhs }
                if checked.types[lhs] == Ty::String && compare(op).is_some() =>
            {
                let left = self.expr(lhs, None)?;
                let right = self.expr(rhs, None)?;
                let slot = self.dst(dst);
                self.asm.emit(
                    Op::StrCompare,
                    Operands::abc(slot.into(), left.into(), right.into()),
                    span,
                )?;
                let zero = self.temp();
                self.asm.load(zero, 0, ValueType::I64, span)?;
                let (opcode, left, right) = match op {
                    BinaryOp::Eq => (Op::CsetEq, slot, zero),
                    BinaryOp::NotEq => (Op::CsetNe, slot, zero),
                    BinaryOp::Lt => (Op::CsetLt, slot, zero),
                    BinaryOp::LtEq => (Op::CsetLe, slot, zero),
                    BinaryOp::Gt => (Op::CsetLt, zero, slot),
                    BinaryOp::GtEq => (Op::CsetLe, zero, slot),
                    _ => unreachable!(),
                };
                self.asm.emit(
                    opcode,
                    Operands::abc(slot.into(), left.into(), right.into()),
                    span,
                )?;
                Ok(slot)
            }
            Expr::Binary {
                op: BinaryOp::Eq | BinaryOp::NotEq,
                lhs,
                rhs,
            } if matches!(checked.types[lhs], Ty::Class(_) | Ty::OptionalClass(_)) => {
                let left = self.expr(lhs, None)?;
                let right = self.expr(rhs, None)?;
                let slot = self.dst(dst);
                self.asm.emit(
                    Op::RecordEq,
                    Operands::abc(slot.into(), left.into(), right.into()),
                    span,
                )?;
                if matches!(
                    ast.exprs[e],
                    Expr::Binary {
                        op: BinaryOp::NotEq,
                        ..
                    }
                ) {
                    let zero = self.temp();
                    self.asm.load(zero, 0, ValueType::Bool, span)?;
                    self.asm.emit(
                        Op::CsetEq,
                        Operands::abc(slot.into(), slot.into(), zero.into()),
                        span,
                    )?;
                }
                Ok(slot)
            }
            Expr::Binary {
                op: BinaryOp::Eq | BinaryOp::NotEq,
                lhs,
                rhs,
            } if (matches!(
                checked.types[lhs],
                Ty::FixedI64(_)
                    | Ty::FixedString(_)
                    | Ty::FixedScalar(_, _)
                    | Ty::FixedClass(_, _)
                    | Ty::FixedStruct(_, _)
                    | Ty::FixedEnum(_, _)
                    | Ty::Result(_)
                    | Ty::Struct(_)
                    | Ty::Enum(_)
            ) || checked.types[lhs].tagged_optional_inner().is_some())
                || (checked.optional_wrap[lhs] && checked.types[lhs] != Ty::String) =>
            {
                self.aggregate_equality(
                    lhs,
                    rhs,
                    if checked.optional_wrap[lhs] {
                        if checked.types[lhs] == Ty::FindOffset {
                            Ty::OptionalI64
                        } else {
                            checked.types[lhs].optional_of().unwrap()
                        }
                    } else {
                        checked.types[lhs]
                    },
                    matches!(
                        ast.exprs[e],
                        Expr::Binary {
                            op: BinaryOp::NotEq,
                            ..
                        }
                    ),
                    dst,
                    span,
                )
            }
            Expr::Binary {
                op: BinaryOp::Coalesce,
                ..
            } => self.coalesce(e, dst),
            Expr::Binary { .. } => self.chain(e, dst),
            Expr::Call { args, .. } if matches!(checked.resolutions[e], Res::StringConvert(_)) => {
                let Res::StringConvert(from) = checked.resolutions[e] else {
                    unreachable!()
                };
                let value = self.expr(ast.args(args)[0], None)?;
                let slot = self.dst(dst);
                let op = string_format_op(from);
                self.asm
                    .emit(op, Operands::abc(slot.into(), value.into(), 0), span)?;
                Ok(slot)
            }
            Expr::Call { args, .. } if matches!(checked.resolutions[e], Res::Convert { .. }) => {
                self.conversion(e, ast.args(args)[0], dst)
            }
            Expr::Convert { value, .. } => self.conversion(e, value, dst),
            Expr::Call { callee, .. }
                if matches!(checked.resolutions[e], Res::StringLen | Res::StringIsEmpty) =>
            {
                let Expr::Member { base, .. } = ast.exprs[callee] else {
                    unreachable!()
                };
                let src = self.expr(base, None)?;
                let slot = self.dst(dst);
                self.asm
                    .emit(Op::StrLen, Operands::abc(slot.into(), src.into(), 0), span)?;
                if checked.resolutions[e] == Res::StringIsEmpty {
                    let zero = self.temp();
                    self.asm.load(zero, 0, ValueType::I64, span)?;
                    self.asm.emit(
                        Op::CsetEq,
                        Operands::abc(slot.into(), slot.into(), zero.into()),
                        span,
                    )?;
                }
                Ok(slot)
            }
            Expr::Call { callee, .. }
                if matches!(
                    checked.resolutions[e],
                    Res::StringTrim
                        | Res::StringTrimStart
                        | Res::StringTrimEnd
                        | Res::StringUpper
                        | Res::StringLower
                ) =>
            {
                let Expr::Member { base, .. } = ast.exprs[callee] else {
                    unreachable!()
                };
                let src = self.expr(base, None)?;
                let slot = self.dst(dst);
                let op = match checked.resolutions[e] {
                    Res::StringUpper => Op::StrUpper,
                    Res::StringLower => Op::StrLower,
                    Res::StringTrim => Op::StrTrim,
                    Res::StringTrimStart => Op::StrTrimStart,
                    Res::StringTrimEnd => Op::StrTrimEnd,
                    _ => unreachable!(),
                };
                self.asm
                    .emit(op, Operands::abc(slot.into(), src.into(), 0), span)?;
                Ok(slot)
            }
            Expr::Call { callee, args }
                if matches!(
                    checked.resolutions[e],
                    Res::StringSlice
                        | Res::StringReplace
                        | Res::StringContains
                        | Res::StringStarts
                        | Res::StringEnds
                        | Res::StringFind
                ) =>
            {
                let Expr::Member { base, .. } = ast.exprs[callee] else {
                    unreachable!()
                };
                if matches!(
                    checked.resolutions[e],
                    Res::StringSlice | Res::StringReplace
                ) {
                    let first = self.top;
                    self.top += 3;
                    let end = self.top;
                    self.expr(base, Some(first))?;
                    self.top = end;
                    for (index, &argument) in ast.args(args).iter().enumerate() {
                        self.expr(argument, Some(first + index as u32 + 1))?;
                        self.top = end;
                    }
                    let slot = self.dst(dst);
                    self.asm.emit(
                        if checked.resolutions[e] == Res::StringSlice {
                            Op::StrSlice
                        } else {
                            Op::StrReplace
                        },
                        Operands::abc(slot.into(), first.into(), 0),
                        span,
                    )?;
                    Ok(slot)
                } else {
                    let left = self.expr(base, None)?;
                    let right = self.expr(ast.args(args)[0], None)?;
                    let slot = self.dst(dst);
                    let op = match checked.resolutions[e] {
                        Res::StringContains => Op::StrContains,
                        Res::StringStarts => Op::StrStarts,
                        Res::StringEnds => Op::StrEnds,
                        Res::StringFind => Op::StrFind,
                        _ => unreachable!(),
                    };
                    self.asm.emit(
                        op,
                        Operands::abc(slot.into(), left.into(), right.into()),
                        span,
                    )?;
                    Ok(slot)
                }
            }
            Expr::Call { .. } => self.call(e, dst),
            Expr::If(id) => self.if_value(id, dst, span, checked.types[e]),
            Expr::Match(id) => self.match_value(id, dst, span, checked.types[e]),
            Expr::EnumShorthand(_) | Expr::NamedCall { .. } | Expr::Member { .. } | Expr::Error => {
                unreachable!("checked code has no bare members or errors")
            }
        }
    }

    fn interpolation(
        &mut self,
        root: ExprId,
        parts: eitri_base::List<ExprId>,
        dst: Option<u32>,
    ) -> Lowered<u32> {
        use std::fmt::Write;
        let ast = self.input.ast;
        let parts = ast.args(parts);
        let count = u32::try_from(
            parts
                .iter()
                .filter(|&&part| !matches!(ast.exprs[part], Expr::StringPart(_)))
                .count(),
        )
        .map_err(|_| AsmError::CodeTooLarge)?;
        let first = self.top;
        self.top = first.checked_add(count).ok_or(AsmError::CodeTooLarge)?;
        let end = self.top;
        let mut plan = String::new();
        let mut index = 0u32;
        for &part in parts {
            if matches!(ast.exprs[part], Expr::StringPart(_)) {
                let Res::StringLiteral(literal) = self.checked.resolutions[part] else {
                    unreachable!()
                };
                let text = &self.checked.strings[literal as usize];
                write!(plan, "L{:08x}", text.len()).expect("writing to a string");
                plan.push_str(text);
                continue;
            }
            let target = first + index;
            let ty = self.checked.types[part];
            plan.push(match ty {
                Ty::String => 's',
                Ty::I64 => 'i',
                Ty::I8 => 'b',
                Ty::U8 => 'u',
                Ty::U64 => 'U',
                Ty::F32 => 'f',
                Ty::F64 => 'F',
                Ty::Bool => 't',
                Ty::Char => 'c',
                _ => unreachable!("checked formattable type"),
            });
            self.expr(part, Some(target))?;
            self.top = end;
            index += 1;
        }
        let plan_index = self
            .checked
            .strings
            .len()
            .checked_add(self.plans.len())
            .ok_or(AsmError::CodeTooLarge)?;
        self.plans.push(plan);
        let slot = self.dst(dst);
        self.asm.emit(
            Op::StrInterpolate,
            Operands::abc(slot.into(), first.into(), plan_index as i64),
            ast.span(root),
        )?;
        Ok(slot)
    }

    fn coalesce(&mut self, root: ExprId, dst: Option<u32>) -> Lowered<u32> {
        let ast = self.input.ast;
        let mark = self.stack.len();
        let mut node = root;
        while let Expr::Binary {
            op: BinaryOp::Coalesce,
            lhs,
            rhs,
        } = ast.exprs[node]
        {
            self.stack.push(lhs);
            node = rhs;
        }
        let count = self.stack.len() - mark;
        let slot = if self.checked.types[root].tagged_optional_inner().is_some()
            || matches!(
                self.checked.types[root],
                Ty::FixedI64(_)
                    | Ty::FixedString(_)
                    | Ty::FixedScalar(_, _)
                    | Ty::FixedClass(_, _)
                    | Ty::FixedStruct(_, _)
                    | Ty::FixedEnum(_, _)
                    | Ty::Result(_)
                    | Ty::Struct(_)
                    | Ty::Enum(_)
            ) {
            if let Some(dst) = dst {
                dst
            } else {
                let slot = self.top;
                self.top += self.width(self.checked.types[root]);
                slot
            }
        } else {
            self.dst(dst)
        };
        let temporary = self.top;
        let end = self.asm.label();
        for index in 0..count {
            let operand = self.stack[mark + index];
            let found = self.expr(operand, None)?;
            let next = self.asm.label();
            if self.checked.types[operand]
                .tagged_optional_inner()
                .is_some()
                || (self.checked.types[operand] == Ty::FindOffset
                    && self.checked.optional_wrap[operand])
            {
                self.asm
                    .branch_test(Test::False, found, next, ast.span(operand));
                if self.checked.types[root].tagged_optional_inner().is_some() {
                    for offset in 0..self.width(self.checked.types[root]) {
                        self.asm.emit(
                            Op::Mov,
                            Operands::abc((slot + offset).into(), (found + offset).into(), 0),
                            ast.span(operand),
                        )?;
                    }
                } else {
                    let inner = self.checked.types[root];
                    if matches!(
                        inner,
                        Ty::FixedI64(_)
                            | Ty::FixedString(_)
                            | Ty::FixedScalar(_, _)
                            | Ty::FixedClass(_, _)
                            | Ty::FixedStruct(_, _)
                            | Ty::FixedEnum(_, _)
                            | Ty::Result(_)
                            | Ty::Struct(_)
                            | Ty::Enum(_)
                    ) {
                        self.move_nullable(inner, found + 1, slot, false, ast.span(operand))?;
                    } else {
                        self.asm.emit(
                            Op::Mov,
                            Operands::abc(slot.into(), (found + 1).into(), 0),
                            ast.span(operand),
                        )?;
                    }
                }
            } else if let Ty::OptionalClass(id) = self.checked.types[operand] {
                let null = self.temp();
                let equal = self.temp();
                self.asm.emit(
                    Op::RecordNull,
                    Operands::abc(null.into(), 0, id.as_u32().into()),
                    ast.span(operand),
                )?;
                self.asm.emit(
                    Op::RecordEq,
                    Operands::abc(equal.into(), found.into(), null.into()),
                    ast.span(operand),
                )?;
                self.asm
                    .branch_test(Test::True, equal, next, ast.span(operand));
                let op = if matches!(self.checked.types[root], Ty::OptionalClass(_)) {
                    Op::Mov
                } else {
                    Op::RecordUnwrap
                };
                self.asm.emit(
                    op,
                    Operands::abc(slot.into(), found.into(), 0),
                    ast.span(operand),
                )?;
            } else if matches!(self.checked.types[root], Ty::String | Ty::OptionalString) {
                let null = self.temp();
                let equal = self.temp();
                self.asm.emit(
                    Op::NullableNull,
                    Operands::abc(null.into(), 0, 0),
                    ast.span(operand),
                )?;
                self.asm.emit(
                    Op::NullableEq,
                    Operands::abc(equal.into(), found.into(), null.into()),
                    ast.span(operand),
                )?;
                self.asm
                    .branch_test(Test::True, equal, next, ast.span(operand));
                let op = if self.checked.types[root] == Ty::OptionalString {
                    Op::Mov
                } else {
                    Op::NullableUnwrap
                };
                self.asm.emit(
                    op,
                    Operands::abc(slot.into(), found.into(), 0),
                    ast.span(operand),
                )?;
            } else {
                self.asm
                    .branch_imm(Cond::Eq, found, -1, next, ast.span(operand))?;
                if slot != found {
                    self.asm.emit(
                        Op::Mov,
                        Operands::abc(slot.into(), found.into(), 0),
                        ast.span(operand),
                    )?;
                }
            }
            self.asm.jump(end, ast.span(operand));
            self.asm.bind(next)?;
            self.top = temporary;
        }
        self.stack.truncate(mark);
        self.expr(node, Some(slot))?;
        self.asm.bind(end)?;
        Ok(slot)
    }

    /// Flatten concatenation in source order and evaluate every part before
    /// allocating the result. The contiguous window is also the root set at
    /// the join safepoint.
    fn string_join(&mut self, root: ExprId, dst: Option<u32>) -> Lowered<u32> {
        let ast = self.input.ast;
        let mark = self.stack.len();
        self.stack.push(root);
        let mut count = 0u32;
        while self.stack.len() > mark {
            let node = self.stack.pop().expect("above mark");
            if let Expr::Binary {
                op: BinaryOp::Add,
                lhs,
                rhs,
            } = ast.exprs[node]
                && self.checked.types[node] == Ty::String
            {
                self.stack.push(rhs);
                self.stack.push(lhs);
            } else {
                count = count.checked_add(1).ok_or(AsmError::CodeTooLarge)?;
            }
        }
        let first = self.top;
        self.top = first.checked_add(count).ok_or(AsmError::CodeTooLarge)?;
        let end = self.top;
        let mut index = 0;
        self.stack.push(root);
        while self.stack.len() > mark {
            let node = self.stack.pop().expect("above mark");
            if let Expr::Binary {
                op: BinaryOp::Add,
                lhs,
                rhs,
            } = ast.exprs[node]
                && self.checked.types[node] == Ty::String
            {
                self.stack.push(rhs);
                self.stack.push(lhs);
            } else {
                self.expr(node, Some(first + index))?;
                self.top = end;
                index += 1;
            }
        }
        let slot = self.dst(dst);
        self.asm.emit(
            Op::StrJoin,
            Operands::abc(slot.into(), first.into(), (end - 1).into()),
            ast.span(root),
        )?;
        Ok(slot)
    }

    /// `&&`/`||` as a value: branch, then write `true` or `false`.
    fn logic_value(&mut self, e: ExprId, dst: Option<u32>) -> Lowered<u32> {
        let span = self.input.ast.span(e);
        let no = self.asm.label();
        let end = self.asm.label();
        self.branch(e, false, no)?;
        let slot = self.dst(dst);
        self.asm.load(slot, 1, ValueType::Bool, span)?;
        self.asm.jump(end, span);
        self.asm.bind(no)?;
        self.asm.load(slot, 0, ValueType::Bool, span)?;
        self.asm.bind(end)?;
        Ok(slot)
    }

    /// Arithmetic and comparisons along a left spine, iteratively: the
    /// left-most operand first, then each operator from the inside out,
    /// with one accumulator for intermediate results and each right
    /// operand's temporaries released after use.
    fn chain(&mut self, root: ExprId, dst: Option<u32>) -> Lowered<u32> {
        let ast = self.input.ast;
        let mark = self.stack.len();
        let mut node = root;
        while let Expr::Binary { op, lhs, .. } = ast.exprs[node]
            && !matches!(op, BinaryOp::And | BinaryOp::Or | BinaryOp::Coalesce)
        {
            self.stack.push(node);
            node = lhs;
        }
        let mut acc = self.expr(node, None)?;
        // Intermediate results never land in `dst`: it may be a binding a
        // later operand still reads.
        let accumulator = (self.stack.len() - mark > 1).then(|| self.temp());
        while self.stack.len() > mark {
            let n = self.stack.pop().expect("above the mark");
            let Expr::Binary { op, rhs, .. } = ast.exprs[n] else {
                unreachable!("only binary nodes are pushed");
            };
            // A small literal right operand becomes an immediate (E5, C2).
            let Expr::Binary { lhs, .. } = ast.exprs[n] else {
                unreachable!()
            };
            let operand_ty = self.checked.types[lhs];
            let imm = self
                .imm8(rhs)
                .filter(|_| operand_ty == Ty::I64)
                .zip(imm_op(op));
            let save = self.top;
            let rhs = match imm {
                Some(_) => 0,
                None => self.expr(rhs, None)?,
            };
            // The right operand is read by this one instruction, so its
            // temporaries are free again before the result is placed.
            self.top = save;
            let out = if n == root {
                self.dst(dst)
            } else {
                accumulator.expect("intermediate results need an accumulator")
            };
            match imm {
                Some((k, imm_op)) => {
                    let operands = Operands::abc(out.into(), acc.into(), k);
                    self.asm.emit(imm_op, operands, ast.span(n))?;
                }
                None => self.operate(op, operand_ty, out, acc, rhs, ast.span(n))?,
            }
            acc = out;
        }
        Ok(acc)
    }

    /// An integer literal that fits an eight-bit immediate.
    fn imm8(&self, e: ExprId) -> Option<i64> {
        if SIMPLE {
            return None;
        }
        match self.input.ast.exprs[e] {
            Expr::Int { .. } => self
                .checked
                .int_value(e)
                .filter(|k| (i8::MIN as i64..=i8::MAX as i64).contains(k)),
            _ => None,
        }
    }

    /// A comparison with a small literal on either side as a `CBI`
    /// condition on the other operand, when the condition and its
    /// complement both fit (`x <= k` is `x < k + 1`). The literal has no
    /// effects, so evaluating only the other operand keeps source order.
    fn compare_imm(&self, op: BinaryOp, lhs: ExprId, rhs: ExprId) -> Option<(ExprId, Cond, i64)> {
        let (x, k, op) = match self.imm8(rhs) {
            Some(k) => (lhs, k, op),
            None => {
                let mirrored = match op {
                    BinaryOp::Lt => BinaryOp::Gt,
                    BinaryOp::LtEq => BinaryOp::GtEq,
                    BinaryOp::Gt => BinaryOp::Lt,
                    BinaryOp::GtEq => BinaryOp::LtEq,
                    other => other,
                };
                (rhs, self.imm8(lhs)?, mirrored)
            }
        };
        let (cond, k) = match op {
            BinaryOp::Eq => (Cond::Eq, k),
            BinaryOp::NotEq => (Cond::Ne, k),
            BinaryOp::Lt => (Cond::Lt, k),
            BinaryOp::LtEq => (Cond::Lt, k + 1),
            BinaryOp::Gt => (Cond::Gt, k),
            BinaryOp::GtEq => (Cond::Gt, k - 1),
            _ => return None,
        };
        imm_complement(cond, k)?;
        Some((x, cond, k))
    }

    fn conversion(&mut self, e: ExprId, value: ExprId, dst: Option<u32>) -> Lowered<u32> {
        let Res::Convert { from, to, checked } = self.checked.resolutions[e] else {
            unreachable!()
        };
        let source = self.expr(value, None)?;
        let out = self.dst(dst);
        self.convert(out, source, from, to, checked, self.input.ast.span(e))?;
        Ok(out)
    }

    fn convert(
        &mut self,
        out: u32,
        source: u32,
        from: Ty,
        to: Ty,
        checked: bool,
        span: Span,
    ) -> Lowered<()> {
        use eitri_bytecode::scalar::integer_format;
        if from == to {
            if out != source {
                self.asm
                    .emit(Op::Mov, Operands::abc(out.into(), source.into(), 0), span)?;
            }
            return Ok(());
        }
        let (op, format) = if from.integer() && to.integer() {
            (
                if checked { Op::CvtChk } else { Op::CvtWrap },
                integer_format(to.bits(), to.signed()) | (u8::from(from.signed()) << 3),
            )
        } else if from.integer() {
            (
                if to == Ty::F32 {
                    Op::FcvtI2f32
                } else {
                    Op::FcvtI2f64
                },
                integer_format(from.bits(), from.signed()),
            )
        } else if to.integer() {
            (
                if from == Ty::F32 {
                    Op::FcvtF32i
                } else {
                    Op::FcvtF64i
                },
                integer_format(to.bits(), to.signed()),
            )
        } else {
            (
                if to == Ty::F32 {
                    Op::FcvtF64f32
                } else {
                    Op::FcvtF32f64
                },
                0,
            )
        };
        self.asm.emit(
            op,
            Operands::abc(out.into(), source.into(), format.into()),
            span,
        )
    }

    fn operate(
        &mut self,
        op: BinaryOp,
        ty: Ty,
        out: u32,
        a: u32,
        b: u32,
        span: Span,
    ) -> Lowered<()> {
        use BinaryOp::*;
        let (opcode, a, b) = if ty.float() {
            let f32 = ty == Ty::F32;
            let (opcode, swap) = match op {
                Add => (if f32 { Op::FaddF32 } else { Op::FaddF64 }, false),
                Sub => (if f32 { Op::FsubF32 } else { Op::FsubF64 }, false),
                Mul => (if f32 { Op::FmulF32 } else { Op::FmulF64 }, false),
                Div => (if f32 { Op::FdivF32 } else { Op::FdivF64 }, false),
                Rem => (if f32 { Op::FremF32 } else { Op::FremF64 }, false),
                Eq => (
                    if f32 {
                        Op::FcsetFeqF32
                    } else {
                        Op::FcsetFeqF64
                    },
                    false,
                ),
                NotEq => (
                    if f32 {
                        Op::FcsetFneF32
                    } else {
                        Op::FcsetFneF64
                    },
                    false,
                ),
                Lt | Gt => (
                    if f32 {
                        Op::FcsetFltF32
                    } else {
                        Op::FcsetFltF64
                    },
                    op == Gt,
                ),
                LtEq | GtEq => (
                    if f32 {
                        Op::FcsetFleF32
                    } else {
                        Op::FcsetFleF64
                    },
                    op == GtEq,
                ),
                _ => unreachable!(),
            };
            (opcode, if swap { b } else { a }, if swap { a } else { b })
        } else {
            let narrow = ty == Ty::I8 || ty == Ty::U8;
            let unsigned = ty == Ty::U64 || ty == Ty::U8 || ty == Ty::Char;
            let opcode = match op {
                Add => {
                    if narrow {
                        Op::AddWrap
                    } else if unsigned {
                        Op::AddChkU
                    } else {
                        Op::AddChkS
                    }
                }
                Sub => {
                    if narrow {
                        Op::SubWrap
                    } else if unsigned {
                        Op::SubChkU
                    } else {
                        Op::SubChkS
                    }
                }
                Mul => {
                    if narrow {
                        Op::MulWrap
                    } else if unsigned {
                        Op::MulChkU
                    } else {
                        Op::MulChkS
                    }
                }
                WrapAdd => Op::AddWrap,
                WrapSub => Op::SubWrap,
                WrapMul => Op::MulWrap,
                Div => {
                    if unsigned {
                        Op::DivU
                    } else {
                        Op::DivS
                    }
                }
                Rem => {
                    if unsigned {
                        Op::RemU
                    } else {
                        Op::RemS
                    }
                }
                BitAnd => Op::And,
                BitOr => Op::Or,
                BitXor => Op::Xor,
                Shl => match ty {
                    Ty::I8 => Op::ShlS8,
                    Ty::U8 => Op::ShlU8,
                    _ => Op::Shl64,
                },
                Shr => match ty {
                    Ty::I8 => Op::ShrS8,
                    Ty::U8 => Op::ShrU8,
                    Ty::U64 => Op::ShrU64,
                    _ => Op::ShrS64,
                },
                Eq => Op::CsetEq,
                NotEq => Op::CsetNe,
                Lt | Gt => {
                    if unsigned {
                        Op::CsetLo
                    } else {
                        Op::CsetLt
                    }
                }
                LtEq | GtEq => {
                    if unsigned {
                        Op::CsetLs
                    } else {
                        Op::CsetLe
                    }
                }
                And | Or | Coalesce => unreachable!(),
            };
            let swap = matches!(op, Gt | GtEq);
            (opcode, if swap { b } else { a }, if swap { a } else { b })
        };
        self.asm
            .emit(opcode, Operands::abc(out.into(), a.into(), b.into()), span)?;
        if matches!(
            op,
            Add | Sub | Mul | Div | Rem | WrapAdd | WrapSub | WrapMul
        ) && matches!(ty, Ty::I8 | Ty::U8 | Ty::U64)
        {
            let from = if (matches!(op, Div | Rem) && !ty.signed())
                || (ty == Ty::U64 && matches!(op, Add | Sub | Mul))
            {
                Ty::U64
            } else {
                Ty::I64
            };
            self.convert(
                out,
                out,
                from,
                ty,
                !matches!(op, WrapAdd | WrapSub | WrapMul),
                span,
            )?;
        }
        Ok(())
    }

    /// Arguments in order into a fresh window; the result after it, so a
    /// host's argument and result windows never overlap.
    fn call(&mut self, e: ExprId, dst: Option<u32>) -> Lowered<u32> {
        let (ast, checked) = (self.input.ast, self.checked);
        let Expr::Call { callee, args } = ast.exprs[e] else {
            unreachable!("called on a call");
        };
        let (target, returns) = match checked.resolutions[e] {
            Res::Function(f)
            | Res::Method(f)
            | Res::MutMethod(f, _)
            | Res::BareMethod(f, _)
            | Res::BareMutMethod(f, _) => (
                CallTarget::Script(FunctionId::from_u32(f.as_u32() + 1)),
                checked.signatures[f].ret != Ty::Void,
            ),
            Res::Host(h) => (
                CallTarget::Host(h.as_u32()),
                self.input.hosts.function(h).ret != Ty::Void,
            ),
            _ => unreachable!("checked calls resolve to functions"),
        };
        let args = ast.args(args);
        let receiver = if matches!(
            checked.resolutions[e],
            Res::Method(_) | Res::MutMethod(_, _)
        ) {
            let Expr::Member { base, .. } = ast.exprs[callee] else {
                unreachable!()
            };
            Some(base)
        } else {
            None
        };
        let class_root = if matches!(checked.resolutions[e], Res::MutMethod(_, _)) {
            receiver.and_then(|receiver| self.class_field_root(receiver))
        } else {
            None
        };
        let captured_object = class_root.map(|_| self.temp());
        let first = self.top;
        let bare_receiver = match checked.resolutions[e] {
            Res::BareMethod(_, binding) | Res::BareMutMethod(_, binding) => Some(binding),
            _ => None,
        };
        let receiver_width = if let Some(binding) = bare_receiver {
            self.width(checked.bindings[binding].ty)
        } else {
            receiver.map_or(0, |base| self.width(checked.types[base]))
        };
        let argument_slots: u32 = receiver_width
            + args
                .iter()
                .map(|&arg| {
                    if checked.optional_wrap[arg]
                        && !matches!(checked.types[arg], Ty::String | Ty::Class(_))
                    {
                        1 + self.width(checked.types[arg])
                    } else {
                        self.width(checked.types[arg])
                    }
                })
                .sum::<u32>();
        self.top += argument_slots;
        if let Some(receiver) = receiver {
            let save = self.top;
            if let Some((base, implicit, owner, index, root)) = class_root {
                let object = captured_object.unwrap();
                if let Some(base) = base {
                    self.expr(base, Some(object))?;
                } else {
                    self.asm.emit(
                        Op::Mov,
                        Operands::abc(object.into(), self.slots[implicit.unwrap()].into(), 0),
                        ast.span(receiver),
                    )?;
                }
                let root_ty = checked.types[root];
                let root_slot =
                    self.class_read_field(object, owner, index, root_ty, None, ast.span(receiver))?;
                let nested_offset = self.class_nested_offset(receiver, root);
                for part in 0..receiver_width {
                    self.asm.emit(
                        Op::Mov,
                        Operands::abc(
                            (first + part).into(),
                            (root_slot + nested_offset + part).into(),
                            0,
                        ),
                        ast.span(receiver),
                    )?;
                }
            } else {
                self.expr(receiver, Some(first))?;
            }
            self.top = save;
        }
        if let Some(binding) = bare_receiver {
            for offset in 0..receiver_width {
                self.asm.emit(
                    Op::Mov,
                    Operands::abc(
                        (first + offset).into(),
                        (self.slots[binding] + offset).into(),
                        0,
                    ),
                    ast.span(e),
                )?;
            }
        }
        let mut offset = receiver_width;
        for &arg in args {
            let save = self.top;
            self.expr(arg, Some(first + offset))?;
            self.top = save;
            offset += if checked.optional_wrap[arg]
                && !matches!(checked.types[arg], Ty::String | Ty::Class(_))
            {
                1 + self.width(checked.types[arg])
            } else {
                self.width(checked.types[arg])
            };
        }
        let return_width = if returns {
            self.width(checked.types[e])
        } else {
            0
        };
        let mutation = match checked.resolutions[e] {
            Res::MutMethod(_, binding) | Res::BareMutMethod(_, binding) => Some(binding),
            _ => None,
        };
        let result = if mutation.is_some() {
            let first = self.top;
            self.top += return_width + receiver_width;
            SlotRange {
                first,
                count: return_width + receiver_width,
            }
        } else if returns {
            let slot = match dst {
                Some(slot) => slot,
                None => {
                    let slot = self.top;
                    self.top += return_width;
                    slot
                }
            };
            SlotRange {
                first: slot,
                count: return_width,
            }
        } else {
            SlotRange { first: 0, count: 0 }
        };
        let site = CallSite {
            target,
            args: SlotRange {
                first,
                count: argument_slots,
            },
            result,
        };
        self.asm.call(site, ast.span(e))?;
        if let Some(binding) = mutation {
            if let Some((_, _, owner, index, root)) = class_root {
                let object = captured_object.unwrap();
                let receiver = receiver.unwrap();
                let root_ty = checked.types[root];
                let root_slot =
                    self.class_read_field(object, owner, index, root_ty, None, ast.span(e))?;
                let nested_offset = self.class_nested_offset(receiver, root);
                for offset in 0..receiver_width {
                    self.asm.emit(
                        Op::Mov,
                        Operands::abc(
                            (root_slot + nested_offset + offset).into(),
                            (result.first + return_width + offset).into(),
                            0,
                        ),
                        ast.span(e),
                    )?;
                }
                self.class_store_field(object, owner, index, root_ty, root_slot, ast.span(e))?;
            } else {
                let destination = receiver.map_or(self.slots[binding], |receiver| {
                    self.field_address(receiver, binding)
                });
                for offset in 0..receiver_width {
                    self.asm.emit(
                        Op::Mov,
                        Operands::abc(
                            (destination + offset).into(),
                            (result.first + return_width + offset).into(),
                            0,
                        ),
                        ast.span(e),
                    )?;
                }
            }
            if returns && let Some(destination) = dst {
                for offset in 0..return_width {
                    self.asm.emit(
                        Op::Mov,
                        Operands::abc(
                            (destination + offset).into(),
                            (result.first + offset).into(),
                            0,
                        ),
                        ast.span(e),
                    )?;
                }
                return Ok(destination);
            }
        }
        Ok(result.first)
    }

    fn aggregate_equality(
        &mut self,
        left: ExprId,
        right: ExprId,
        ty: Ty,
        negate: bool,
        dst: Option<u32>,
        span: Span,
    ) -> Lowered<u32> {
        let a = self.top;
        self.top += self.width(ty);
        self.expr(left, Some(a))?;
        let b = self.expr(right, None)?;
        let result = self.dst(dst);
        let temp = self.temp();
        let zero = self.temp();
        self.asm.load(zero, 0, ValueType::I64, span)?;
        let fail = self.asm.label();
        let done = self.asm.label();
        let mut types = self.asm.temporary_vec(self.width(ty) as usize)?;
        self.flatten_type(ty, &mut types)?;
        for (offset, ty) in types.into_iter().enumerate() {
            let (left, right) = (a + offset as u32, b + offset as u32);
            match ty {
                ValueType::String => {
                    self.asm.emit(
                        Op::StrCompare,
                        Operands::abc(temp.into(), left.into(), right.into()),
                        span,
                    )?;
                    self.asm.branch_if(Cond::Ne, temp, zero, fail, span)?;
                }
                ValueType::Record(id) | ValueType::NullableRecord(id) => {
                    self.asm.emit(
                        if id as usize >= self.input.ast.structs.len() {
                            Op::RecordDeepEq
                        } else {
                            Op::RecordEq
                        },
                        Operands::abc(temp.into(), left.into(), right.into()),
                        span,
                    )?;
                    self.asm.branch_test(Test::False, temp, fail, span);
                }
                ValueType::NullableString => {
                    self.asm.emit(
                        Op::NullableEq,
                        Operands::abc(temp.into(), left.into(), right.into()),
                        span,
                    )?;
                    self.asm.branch_test(Test::False, temp, fail, span);
                }
                ValueType::F32 | ValueType::F64 => {
                    let op = if ty == ValueType::F32 {
                        Op::FcsetFeqF32
                    } else {
                        Op::FcsetFeqF64
                    };
                    self.asm.emit(
                        op,
                        Operands::abc(temp.into(), left.into(), right.into()),
                        span,
                    )?;
                    self.asm.branch_test(Test::False, temp, fail, span);
                }
                _ => self.asm.branch_if(Cond::Ne, left, right, fail, span)?,
            }
        }
        self.asm
            .load(result, u64::from(!negate), ValueType::Bool, span)?;
        self.asm.jump(done, span);
        self.asm.bind(fail)?;
        self.asm
            .load(result, u64::from(negate), ValueType::Bool, span)?;
        self.asm.bind(done)?;
        Ok(result)
    }

    fn match_payload_partial(
        &self,
        enum_id: eitri_syntax::ast::EnumId,
        index: usize,
        bindings: eitri_base::List<eitri_syntax::ast::MatchBinding>,
    ) -> bool {
        let ast = self.input.ast;
        let case = ast.enum_cases[ast.enums[enum_id].cases][index];
        let fields = ast.params(case.fields);
        let types =
            self.checked.enum_case_types[self.checked.enum_starts[enum_id] as usize + index];
        ast.match_bindings[bindings]
            .iter()
            .enumerate()
            .any(|(position, pattern)| {
                if pattern.literal.is_some() || pattern.nested_case.is_some() {
                    return true;
                }
                if self.input.tokens.kind(pattern.name) == eitri_syntax::TokenKind::Wildcard {
                    return false;
                }
                let offset = pattern.label.map_or(position, |label| {
                    fields
                        .iter()
                        .position(|field| {
                            self.input.tokens.symbol(field.name) == self.input.tokens.symbol(label)
                        })
                        .expect("checked field label")
                });
                self.checked.param_types[types][offset]
                    .optional_inner()
                    .is_some()
            })
    }

    fn match_present(&mut self, source: u32, ty: Ty, next: Label, span: Span) -> Lowered<u32> {
        match ty {
            ty if ty.tagged_optional_inner().is_some() => {
                self.asm.branch_test(Test::False, source, next, span);
                let inner = ty.tagged_optional_inner().unwrap();
                if matches!(
                    inner,
                    Ty::FixedI64(_)
                        | Ty::FixedString(_)
                        | Ty::FixedScalar(_, _)
                        | Ty::FixedClass(_, _)
                        | Ty::FixedStruct(_, _)
                        | Ty::FixedEnum(_, _)
                        | Ty::Result(_)
                        | Ty::Struct(_)
                        | Ty::Enum(_)
                ) {
                    let value = self.top;
                    self.top += self.width(inner);
                    self.move_nullable(inner, source + 1, value, false, span)?;
                    Ok(value)
                } else {
                    Ok(source + 1)
                }
            }
            Ty::OptionalClass(id) => {
                let null = self.temp();
                let equal = self.temp();
                let value = self.temp();
                self.asm.emit(
                    Op::RecordNull,
                    Operands::abc(null.into(), 0, id.as_u32().into()),
                    span,
                )?;
                self.asm.emit(
                    Op::RecordEq,
                    Operands::abc(equal.into(), source.into(), null.into()),
                    span,
                )?;
                self.asm.branch_test(Test::True, equal, next, span);
                self.asm.emit(
                    Op::RecordUnwrap,
                    Operands::abc(value.into(), source.into(), 0),
                    span,
                )?;
                Ok(value)
            }
            Ty::OptionalString => {
                let null = self.temp();
                let equal = self.temp();
                let value = self.temp();
                self.asm
                    .emit(Op::NullableNull, Operands::abc(null.into(), 0, 0), span)?;
                self.asm.emit(
                    Op::NullableEq,
                    Operands::abc(equal.into(), source.into(), null.into()),
                    span,
                )?;
                self.asm.branch_test(Test::True, equal, next, span);
                self.asm.emit(
                    Op::NullableUnwrap,
                    Operands::abc(value.into(), source.into(), 0),
                    span,
                )?;
                Ok(value)
            }
            _ => Ok(source),
        }
    }

    /// Pattern recursion follows the parser's bounded nesting depth; each
    /// label lookup scans at most 256 fields.
    fn match_payload(
        &mut self,
        subject: u32,
        enum_id: eitri_syntax::ast::EnumId,
        index: usize,
        bindings: eitri_base::List<eitri_syntax::ast::MatchBinding>,
        next: Label,
        span: Span,
    ) -> Lowered<()> {
        let ast = self.input.ast;
        let cases = &ast.enum_cases[ast.enums[enum_id].cases];
        let types =
            self.checked.enum_case_types[self.checked.enum_starts[enum_id] as usize + index];
        let fields = ast.params(cases[index].fields);
        let payload = if cases[index].indirect {
            let record = self.temp();
            self.asm.emit(
                Op::RecordUnwrap,
                Operands::abc(
                    record.into(),
                    (subject + self.enum_case_offset(enum_id, index)).into(),
                    0,
                ),
                span,
            )?;
            let width: u32 = self.checked.param_types[types]
                .iter()
                .map(|&ty| self.width(ty))
                .sum();
            let first = self.top;
            self.top += width;
            for offset in 0..width {
                self.asm.emit(
                    Op::RecordGet,
                    Operands::abc((first + offset).into(), record.into(), offset.into()),
                    span,
                )?;
            }
            Some(first)
        } else {
            None
        };
        for (position, pattern) in ast.match_bindings[bindings].iter().enumerate() {
            let offset = pattern.label.map_or(position, |label| {
                fields
                    .iter()
                    .position(|field| {
                        self.input.tokens.symbol(field.name) == self.input.tokens.symbol(label)
                    })
                    .expect("checked field label")
            });
            if let Some(case) = pattern.nested_case {
                let payload_ty = self.checked.param_types[types][offset];
                let nested = match payload_ty {
                    Ty::Enum(id) | Ty::OptionalEnum(id) => id,
                    _ => unreachable!(),
                };
                let nested_cases = &ast.enum_cases[ast.enums[nested].cases];
                let tag = nested_cases
                    .iter()
                    .position(|candidate| {
                        self.input.tokens.symbol(candidate.name) == self.input.tokens.symbol(case)
                    })
                    .expect("checked nested case");
                let constant = self.temp();
                self.asm.load(constant, tag as u64, ValueType::I64, span)?;
                let source = payload.map_or_else(
                    || subject + self.enum_field_offset(enum_id, index, offset),
                    |first| {
                        first
                            + self.checked.param_types[types][..offset]
                                .iter()
                                .map(|&ty| self.width(ty))
                                .sum::<u32>()
                    },
                );
                let source = self.match_present(source, payload_ty, next, span)?;
                self.asm.branch_if(Cond::Ne, source, constant, next, span)?;
                if let Some(children) = pattern.nested_bindings {
                    self.match_payload(source, nested, tag, children, next, span)?;
                }
            } else if let Some(literal) = pattern.literal {
                let ty = self.checked.param_types[types][offset];
                let source = payload.map_or_else(
                    || subject + self.enum_field_offset(enum_id, index, offset),
                    |first| {
                        first
                            + self.checked.param_types[types][..offset]
                                .iter()
                                .map(|&ty| self.width(ty))
                                .sum::<u32>()
                    },
                );
                if matches!(ast.exprs[literal], Expr::Null(_)) {
                    if let Ty::OptionalClass(id) = ty {
                        let null = self.temp();
                        let equal = self.temp();
                        self.asm.emit(
                            Op::RecordNull,
                            Operands::abc(null.into(), 0, id.as_u32().into()),
                            span,
                        )?;
                        self.asm.emit(
                            Op::RecordEq,
                            Operands::abc(equal.into(), source.into(), null.into()),
                            span,
                        )?;
                        self.asm.branch_test(Test::False, equal, next, span);
                    } else if ty == Ty::OptionalString {
                        let null = self.temp();
                        let equal = self.temp();
                        self.asm
                            .emit(Op::NullableNull, Operands::abc(null.into(), 0, 0), span)?;
                        self.asm.emit(
                            Op::NullableEq,
                            Operands::abc(equal.into(), source.into(), null.into()),
                            span,
                        )?;
                        self.asm.branch_test(Test::False, equal, next, span);
                    } else {
                        self.asm.branch_test(Test::True, source, next, span);
                    }
                    continue;
                }
                let source = self.match_present(source, ty, next, span)?;
                let inner = ty.optional_inner().unwrap_or(ty);
                let source = if ty == Ty::String && payload.is_none() {
                    let slot = self.temp();
                    self.move_nullable(ty, source, slot, false, span)?;
                    slot
                } else {
                    source
                };
                let value = self.expr(literal, None)?;
                if inner == Ty::String {
                    let cmp = self.temp();
                    let zero = self.temp();
                    self.asm.load(zero, 0, ValueType::I64, span)?;
                    self.asm.emit(
                        Op::StrCompare,
                        Operands::abc(cmp.into(), source.into(), value.into()),
                        span,
                    )?;
                    self.asm.branch_if(Cond::Ne, cmp, zero, next, span)?;
                } else if inner.float() {
                    let equal = self.temp();
                    let op = if inner == Ty::F32 {
                        Op::FcsetFeqF32
                    } else {
                        Op::FcsetFeqF64
                    };
                    self.asm.emit(
                        op,
                        Operands::abc(equal.into(), source.into(), value.into()),
                        span,
                    )?;
                    self.asm.branch_test(Test::False, equal, next, span);
                } else {
                    self.asm.branch_if(Cond::Ne, source, value, next, span)?;
                }
            } else if let Some(binding) = self.checked.pattern_bindings[pattern.name] {
                let ty = self.checked.param_types[types][offset];
                let source = payload.map_or_else(
                    || subject + self.enum_field_offset(enum_id, index, offset),
                    |first| {
                        first
                            + self.checked.param_types[types][..offset]
                                .iter()
                                .map(|&ty| self.width(ty))
                                .sum::<u32>()
                    },
                );
                let source = self.match_present(source, ty, next, span)?;
                let target = self.top;
                let narrowed = self.checked.bindings[binding].ty;
                self.top += self.width(narrowed);
                self.slots[binding] = target;
                if ty.optional_inner().is_some() {
                    for offset in 0..self.width(narrowed) {
                        self.asm.emit(
                            Op::Mov,
                            Operands::abc((target + offset).into(), (source + offset).into(), 0),
                            span,
                        )?;
                    }
                } else if payload.is_none() {
                    self.move_nullable(ty, source, target, false, span)?;
                } else {
                    for part in 0..self.width(ty) {
                        self.asm.emit(
                            Op::Mov,
                            Operands::abc((target + part).into(), (source + part).into(), 0),
                            span,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    fn class_field_root(&self, mut expr: ExprId) -> Option<ClassFieldRoot> {
        loop {
            match self.input.ast.exprs[expr] {
                Expr::Member { base, .. } => {
                    if let Ty::Class(owner) = self.checked.types[base] {
                        let Res::Field(index) = self.checked.resolutions[expr] else {
                            return None;
                        };
                        return Some((Some(base), None, owner, index, expr));
                    }
                    expr = base;
                }
                _ => {
                    if let Res::SelfField(binding, index) = self.checked.resolutions[expr]
                        && let Ty::Class(owner) = self.checked.bindings[binding].ty
                    {
                        return Some((None, Some(binding), owner, index, expr));
                    }
                    return None;
                }
            }
        }
    }

    fn class_nested_offset(&self, mut expr: ExprId, root: ExprId) -> u32 {
        let mut offset = 0;
        while expr != root {
            let Expr::Member { base, .. } = self.input.ast.exprs[expr] else {
                unreachable!()
            };
            let Ty::Struct(owner) = self.checked.types[base] else {
                unreachable!()
            };
            let Res::Field(index) = self.checked.resolutions[expr] else {
                unreachable!()
            };
            offset += self.field_offset(owner, index);
            expr = base;
        }
        offset
    }

    fn class_read_field(
        &mut self,
        object: u32,
        owner: eitri_syntax::ast::StructId,
        index: u32,
        ty: Ty,
        dst: Option<u32>,
        span: Span,
    ) -> Lowered<u32> {
        let width = self.width(ty);
        let target = if let Some(dst) = dst {
            dst
        } else {
            let slot = self.top;
            self.top += width;
            slot
        };
        let offset = self.field_offset(owner, index);
        for part in 0..width {
            self.asm.emit(
                Op::RecordGet,
                Operands::abc(
                    (target + part).into(),
                    object.into(),
                    (offset + part).into(),
                ),
                span,
            )?;
        }
        Ok(target)
    }

    fn class_store_field(
        &mut self,
        object: u32,
        owner: eitri_syntax::ast::StructId,
        index: u32,
        ty: Ty,
        source: u32,
        span: Span,
    ) -> Lowered<()> {
        let offset = self.field_offset(owner, index);
        for part in 0..self.width(ty) {
            self.asm.emit(
                Op::RecordSet,
                Operands::abc(
                    object.into(),
                    (source + part).into(),
                    (offset + part).into(),
                ),
                span,
            )?;
        }
        Ok(())
    }

    fn class_literal(
        &mut self,
        id: eitri_syntax::ast::StructId,
        fields: eitri_base::List<eitri_syntax::ast::FieldInit>,
        dst: Option<u32>,
        span: Span,
    ) -> Lowered<u32> {
        let ast = self.input.ast;
        let result = self.dst(dst);
        let declared = ast.params(ast.structs[id].fields);
        let entries = &ast.field_inits[fields];
        let first = self.top;
        self.top += self.checked.struct_widths[id];
        let end = self.top;
        if declared.is_empty() {
            self.asm.load(first, 0, ValueType::I64, span)?;
        }
        for field in entries
            .iter()
            .filter(|field| self.input.tokens.kind(field.name) != eitri_syntax::TokenKind::DotDot)
        {
            let index = declared
                .iter()
                .position(|item| {
                    self.input.tokens.symbol(item.name) == self.input.tokens.symbol(field.name)
                })
                .expect("checked field");
            self.expr(
                field.value,
                Some(first + self.field_offset(id, index as u32)),
            )?;
            self.top = end;
        }
        let base = entries
            .last()
            .filter(|field| self.input.tokens.kind(field.name) == eitri_syntax::TokenKind::DotDot)
            .map(|field| field.value);
        if let Some(base) = base {
            let source = self.expr(base, None)?;
            for (index, field) in declared.iter().enumerate() {
                if !entries.iter().any(|init| {
                    self.input.tokens.kind(init.name) != eitri_syntax::TokenKind::DotDot
                        && self.input.tokens.symbol(init.name)
                            == self.input.tokens.symbol(field.name)
                }) {
                    let offset = self.field_offset(id, index as u32);
                    let ty = self.checked.param_types[self.checked.struct_fields[id]][index];
                    for part in 0..self.width(ty) {
                        self.asm.emit(
                            Op::RecordGet,
                            Operands::abc(
                                (first + offset + part).into(),
                                source.into(),
                                (offset + part).into(),
                            ),
                            span,
                        )?;
                    }
                }
            }
            self.top = end;
        }
        for (index, field) in declared.iter().enumerate() {
            if base.is_none()
                && let Some(default) = field.default
                && !entries.iter().any(|init| {
                    self.input.tokens.kind(init.name) != eitri_syntax::TokenKind::DotDot
                        && self.input.tokens.symbol(init.name)
                            == self.input.tokens.symbol(field.name)
                })
            {
                if self.default_depth >= 16 || self.default_expansions >= 65_536 {
                    return Err(AsmError::CodeTooLarge);
                }
                self.default_depth += 1;
                self.default_expansions += 1;
                let value = self.expr(default, Some(first + self.field_offset(id, index as u32)));
                self.default_depth -= 1;
                value?;
                self.top = end;
            }
        }
        self.asm.emit(
            Op::RecordNew,
            Operands::abc(result.into(), first.into(), id.as_u32().into()),
            span,
        )?;
        Ok(result)
    }

    fn match_scalar(
        &mut self,
        id: eitri_syntax::ast::MatchId,
        result: u32,
        span: Span,
        ty: Ty,
    ) -> Lowered<u32> {
        let ast = self.input.ast;
        let node = ast.matches[id];
        let subject_ty = self.checked.types[node.subject];
        let inner = subject_ty.optional_inner();
        let subject = self.top;
        self.top += self.width(subject_ty);
        self.expr(node.subject, Some(subject))?;
        let end = self.asm.label();
        let all = &ast.match_arms[node.arms];
        let arms = if all.last().is_some_and(|arm| arm.guard.is_some()) {
            &all[..all.len() - 1]
        } else {
            all
        };
        for (index, arm) in arms.iter().enumerate() {
            let mark = self.top;
            let next = self.asm.label();
            if let Some(literal) = arm.literal {
                if matches!(ast.exprs[literal], Expr::Null(_)) {
                    if let Ty::OptionalClass(id) = subject_ty {
                        let null = self.temp();
                        let equal = self.temp();
                        self.asm.emit(
                            Op::RecordNull,
                            Operands::abc(null.into(), 0, id.as_u32().into()),
                            span,
                        )?;
                        self.asm.emit(
                            Op::RecordEq,
                            Operands::abc(equal.into(), subject.into(), null.into()),
                            span,
                        )?;
                        self.asm.branch_test(Test::False, equal, next, span);
                    } else if subject_ty == Ty::OptionalString {
                        let null = self.temp();
                        let equal = self.temp();
                        self.asm
                            .emit(Op::NullableNull, Operands::abc(null.into(), 0, 0), span)?;
                        self.asm.emit(
                            Op::NullableEq,
                            Operands::abc(equal.into(), subject.into(), null.into()),
                            span,
                        )?;
                        self.asm.branch_test(Test::False, equal, next, span);
                    } else {
                        self.asm.branch_test(Test::True, subject, next, span);
                    }
                } else {
                    let source = self.match_present(subject, subject_ty, next, span)?;
                    let compare_ty = inner.unwrap_or(subject_ty);
                    let value = self.expr(literal, None)?;
                    if compare_ty == Ty::String {
                        let cmp = self.temp();
                        let zero = self.temp();
                        self.asm.load(zero, 0, ValueType::I64, span)?;
                        self.asm.emit(
                            Op::StrCompare,
                            Operands::abc(cmp.into(), source.into(), value.into()),
                            span,
                        )?;
                        self.asm.branch_if(Cond::Ne, cmp, zero, next, span)?;
                    } else if compare_ty.float() {
                        let equal = self.temp();
                        let op = if compare_ty == Ty::F32 {
                            Op::FcsetFeqF32
                        } else {
                            Op::FcsetFeqF64
                        };
                        self.asm.emit(
                            op,
                            Operands::abc(equal.into(), source.into(), value.into()),
                            span,
                        )?;
                        self.asm.branch_test(Test::False, equal, next, span);
                    } else {
                        self.asm.branch_if(Cond::Ne, source, value, next, span)?;
                    }
                }
            }
            if let Some(token) = arm.catch_binding {
                let binding = self.checked.pattern_bindings[token].expect("checked binding");
                let source = self.match_present(subject, subject_ty, next, span)?;
                let narrowed = inner.unwrap_or(subject_ty);
                let target = self.top;
                self.top += self.width(narrowed);
                self.slots[binding] = target;
                for offset in 0..self.width(narrowed) {
                    self.asm.emit(
                        Op::Mov,
                        Operands::abc((target + offset).into(), (source + offset).into(), 0),
                        span,
                    )?;
                }
            }
            if let Some(guard) = arm.guard {
                self.branch(guard, false, next)?;
            }
            match arm.body {
                eitri_syntax::ast::MatchBody::Expr(body) => {
                    if ty == Ty::Void {
                        self.discard(body)?;
                    } else {
                        self.expr(body, Some(result))?;
                    }
                }
                eitri_syntax::ast::MatchBody::Block(body) => {
                    if ty == Ty::Void {
                        self.statements(body)?;
                    } else {
                        self.block_value(body, result)?;
                    }
                }
            }
            let terminal =
                matches!(arm.body, eitri_syntax::ast::MatchBody::Block(block) if self.ends(block));
            if !terminal
                && (index + 1 != arms.len()
                    || arm.literal.is_some()
                    || (inner.is_some() && arm.catch_binding.is_some()))
            {
                self.asm.jump(end, span);
            }
            self.asm.bind(next)?;
            if index + 1 == arms.len()
                && (arm.literal.is_some() || (inner.is_some() && arm.catch_binding.is_some()))
            {
                self.asm.jump(next, span);
            }
            self.top = mark;
        }
        self.asm.bind(end)?;
        Ok(result)
    }

    fn match_result(
        &mut self,
        id: eitri_syntax::ast::MatchId,
        result_id: u32,
        result: u32,
        span: Span,
        ty: Ty,
    ) -> Lowered<u32> {
        let ast = self.input.ast;
        let node = ast.matches[id];
        let subject = self.top;
        self.top += self.width(Ty::Result(result_id));
        self.expr(node.subject, Some(subject))?;
        let end = self.asm.label();
        let arms = &ast.match_arms[node.arms];
        let arms = if arms.last().is_some_and(|arm| arm.guard.is_some()) {
            &arms[..arms.len() - 1]
        } else {
            arms
        };
        let (error, success) = self.checked.result_types[result_id as usize];
        for (index, arm) in arms.iter().enumerate() {
            let mark = self.top;
            let next = self.asm.label();
            if let Some(case) = arm.case {
                let is_ok = self.input.interner.resolve(self.input.tokens.symbol(case)) == "Ok";
                if index + 1 != arms.len() {
                    self.asm
                        .branch_imm(Cond::Ne, subject, i64::from(is_ok), next, span)?;
                }
                if let Some(pattern) = ast.match_bindings[arm.bindings].first()
                    && let Some(binding) = self.checked.pattern_bindings[pattern.name]
                {
                    let payload_ty = if is_ok { success } else { error };
                    let source = subject + 1 + if is_ok { self.width(error) } else { 0 };
                    let target = self.top;
                    self.top += self.width(payload_ty);
                    self.slots[binding] = target;
                    self.move_nullable(payload_ty, source, target, false, span)?;
                }
            }
            if let Some(token) = arm.catch_binding {
                let binding = self.checked.pattern_bindings[token].expect("checked result binding");
                let target = self.top;
                let width = self.width(Ty::Result(result_id));
                self.top += width;
                self.slots[binding] = target;
                for offset in 0..width {
                    self.asm.emit(
                        Op::Mov,
                        Operands::abc((target + offset).into(), (subject + offset).into(), 0),
                        span,
                    )?;
                }
            }
            if let Some(guard) = arm.guard {
                self.branch(guard, false, next)?;
            }
            match arm.body {
                eitri_syntax::ast::MatchBody::Expr(body) => {
                    if ty == Ty::Void {
                        self.discard(body)?;
                    } else {
                        self.expr(body, Some(result))?;
                    }
                }
                eitri_syntax::ast::MatchBody::Block(body) => {
                    if ty == Ty::Void {
                        self.statements(body)?;
                    } else {
                        self.block_value(body, result)?;
                    }
                }
            }
            let terminal =
                matches!(arm.body, eitri_syntax::ast::MatchBody::Block(block) if self.ends(block));
            if !terminal && index + 1 != arms.len() {
                self.asm.jump(end, span);
            }
            self.asm.bind(next)?;
            self.top = mark;
        }
        self.asm.bind(end)?;
        Ok(result)
    }

    fn match_value(
        &mut self,
        id: eitri_syntax::ast::MatchId,
        dst: Option<u32>,
        span: Span,
        ty: Ty,
    ) -> Lowered<u32> {
        let ast = self.input.ast;
        let node = ast.matches[id];
        let result = if ty == Ty::Void {
            0
        } else {
            match dst {
                Some(slot) => slot,
                None => {
                    let slot = self.top;
                    self.top += self.width(ty);
                    slot
                }
            }
        };
        let subject_ty = self.checked.types[node.subject];
        if let Ty::Result(result_id) = subject_ty {
            return self.match_result(id, result_id, result, span, ty);
        }
        let (enum_id, optional) = match subject_ty {
            Ty::Enum(id) => (id, false),
            Ty::OptionalEnum(id) => (id, true),
            _ => return self.match_scalar(id, result, span, ty),
        };
        // A guard may mutate the source lvalue. Match the value evaluated
        // before any arm or guard runs, even on the optimized lowering path.
        let subject = self.top;
        self.top += self.width(subject_ty);
        self.expr(node.subject, Some(subject))?;
        let cases = &ast.enum_cases[ast.enums[enum_id].cases];
        let end = self.asm.label();
        let all_arms = &ast.match_arms[node.arms];
        // A final guarded arm is unreachable: preceding unguarded arms must
        // already be exhaustive because guards never contribute coverage.
        let arms = if all_arms.last().is_some_and(|arm| arm.guard.is_some()) {
            &all_arms[..all_arms.len() - 1]
        } else {
            all_arms
        };
        for (arm_index, arm) in arms.iter().enumerate() {
            let next = self.asm.label();
            let mark = self.top;
            if arm.literal.is_some() {
                self.asm.branch_test(Test::True, subject, next, span);
            } else if let Some(case_name) = arm.case {
                let source = if optional {
                    self.match_present(subject, subject_ty, next, span)?
                } else {
                    subject
                };
                let index = cases
                    .iter()
                    .position(|case| {
                        self.input.tokens.symbol(case.name) == self.input.tokens.symbol(case_name)
                    })
                    .expect("checked case");
                if arm_index + 1 != arms.len() {
                    let tag = self.temp();
                    self.asm.load(tag, index as u64, ValueType::I64, span)?;
                    self.asm.branch_if(Cond::Ne, source, tag, next, span)?;
                }
                self.match_payload(source, enum_id, index, arm.bindings, next, span)?;
            }
            if let Some(token) = arm.catch_binding {
                let binding = self.checked.pattern_bindings[token].expect("checked catch binding");
                let source = if optional {
                    self.match_present(subject, subject_ty, next, span)?
                } else {
                    subject
                };
                let width = self.width(Ty::Enum(enum_id));
                let target = self.top;
                self.top += width;
                self.slots[binding] = target;
                for offset in 0..width {
                    self.asm.emit(
                        Op::Mov,
                        Operands::abc((target + offset).into(), (source + offset).into(), 0),
                        span,
                    )?;
                }
            }
            if let Some(guard) = arm.guard {
                self.branch(guard, false, next)?;
            }
            match arm.body {
                eitri_syntax::ast::MatchBody::Expr(body) => {
                    if ty == Ty::Void {
                        self.discard(body)?;
                    } else {
                        self.expr(body, Some(result))?;
                    }
                }
                eitri_syntax::ast::MatchBody::Block(body) => {
                    if ty == Ty::Void {
                        self.statements(body)?;
                    } else {
                        self.block_value(body, result)?;
                    }
                }
            }
            let terminal =
                matches!(arm.body, eitri_syntax::ast::MatchBody::Block(block) if self.ends(block));
            let last_partial = arm_index + 1 == arms.len()
                && ((optional
                    && (arm.literal.is_some()
                        || arm.case.is_some()
                        || arm.catch_binding.is_some()))
                    || arm.case.is_some_and(|name| {
                        let index = cases
                            .iter()
                            .position(|case| {
                                self.input.tokens.symbol(case.name)
                                    == self.input.tokens.symbol(name)
                            })
                            .expect("checked case");
                        self.match_payload_partial(enum_id, index, arm.bindings)
                    }));
            if !terminal && (arm_index + 1 != arms.len() || last_partial) {
                self.asm.jump(end, span);
            }
            self.asm.bind(next)?;
            if last_partial {
                // Exhaustiveness makes this edge unreachable for a valid enum.
                // Keep a real target for the verifier even when all arms return.
                self.asm.jump(next, span);
            }
            self.top = mark;
        }
        self.asm.bind(end)?;
        Ok(result)
    }

    /// An `if` used as a value: every branch writes its final expression
    /// into the same slot. Iterates along the `else if` chain.
    fn if_value(&mut self, id: IfId, dst: Option<u32>, span: Span, ty: Ty) -> Lowered<u32> {
        let ast = self.input.ast;
        let slot = match dst {
            Some(slot) => slot,
            None => {
                let slot = self.top;
                self.top += self.width(ty);
                slot
            }
        };
        let end = self.asm.label();
        let mut current = id;
        loop {
            let IfExpr {
                condition,
                then,
                otherwise,
            } = ast.ifs[current];
            let next = self.asm.label();
            self.branch(condition, false, next)?;
            self.block_value(then, slot)?;
            self.asm.jump(end, span);
            self.asm.bind(next)?;
            match otherwise {
                Else::Block(block) => {
                    self.block_value(block, slot)?;
                    break;
                }
                Else::If(e) => {
                    let Expr::If(nested) = ast.exprs[e] else {
                        unreachable!("`else if` holds an `if`");
                    };
                    current = nested;
                }
                Else::None => unreachable!("the checker requires an `else` on a value `if`"),
            }
        }
        self.asm.bind(end)?;
        Ok(slot)
    }

    /// A block used as a value: its statements, then its final expression
    /// into `slot`.
    fn block_value(&mut self, block: Block, slot: u32) -> Lowered<()> {
        let ast = self.input.ast;
        let mark = self.top;
        let stmts = ast.block(block);
        let (&last, rest) = stmts
            .split_last()
            .expect("the checker requires a value block to end in an expression");
        for &stmt in rest {
            self.stmt(stmt)?;
        }
        let Stmt::Expr(value) = ast.stmts[last] else {
            unreachable!("the checker requires a value block to end in an expression");
        };
        self.expr(value, Some(slot))?;
        self.top = mark;
        Ok(())
    }
}

/// The `ALUI` form of an arithmetic operator, if it has one.
fn imm_op(op: BinaryOp) -> Option<Op> {
    Some(match op {
        BinaryOp::Add => Op::AddiChkS,
        BinaryOp::Sub => Op::SubiChkS,
        BinaryOp::Mul => Op::MuliChkS,
        BinaryOp::WrapAdd => Op::AddiWrap,
        BinaryOp::WrapSub => Op::SubiWrap,
        _ => return None,
    })
}

/// The register-form condition for a comparison, and whether its operands
/// swap (`a > b` is `b < a`).
fn compare(op: BinaryOp) -> Option<(Cond, bool)> {
    Some(match op {
        BinaryOp::Eq => (Cond::Eq, false),
        BinaryOp::NotEq => (Cond::Ne, false),
        BinaryOp::Lt => (Cond::Lt, false),
        BinaryOp::LtEq => (Cond::Le, false),
        BinaryOp::Gt => (Cond::Lt, true),
        BinaryOp::GtEq => (Cond::Le, true),
        _ => return None,
    })
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod synthetic_roots;
