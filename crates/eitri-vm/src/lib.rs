//! The scalar register interpreter (T006).
//!
//! Portable Rust `match` dispatch over verified code (D9): the baseline that
//! threaded or generated dispatch must beat. Slots are untagged 64-bit words
//! on one growable stack; control frames are a separate stack, so script
//! recursion never recurses in Rust and its depth is a checked limit.
//!
//! - A script call copies arguments from the caller's window into the
//!   callee's frame (results, then parameters, then locals) and `RET`
//!   copies the result span back (D8, non-overlapping frames).
//! - A host call gives the host a [`Call`] over the caller's argument and
//!   result windows, in place. Through it the host may reenter: run a
//!   script function of the same program on top of the current frames,
//!   sharing the slot and frame stacks, the budget, and the depth limit.
//! - Every call and every taken backward branch debits one step of the
//!   budget before it happens; a budget of N permits exactly N debits.
//! - A fault stops the run with its kind and a trace of (function, PC),
//!   innermost first; a fault inside `WIDE` reports the prefix's PC.
//!
//! The loop keeps the current code and frame as local slices, decodes
//! operands inside the handlers that use them, and expands the handlers
//! twice so the common path has no `WIDE` merge: measured in the research
//! ledger (E5/E4 first pass).
//!
//! [`run_threaded`] is the tail-call threaded interpreter with the same
//! contract, and the default since 2026-11-07; this `match` loop remains
//! the portable reference. Both must agree on every test and fixture (E5
//! second pass in the research ledger).
//!
//! The interpreter is safe Rust with bounds-checked slots: even unverified
//! code cannot cause undefined behavior, only a panic. The verifier is what
//! guarantees well-typed code; the interpreter does not re-check types.

#![feature(
    allocator_ext,
    explicit_tail_calls,
    rust_preserve_none_cc,
    core_intrinsics,
    vec_push_within_capacity
)]
#![allow(incomplete_features, internal_features)]

use std::marker::PhantomData;

use eitri_base::IndexVec;
use eitri_bytecode::{CallTarget, Function, FunctionId, Op, SlotRange};

mod heap;
pub use heap::RetainedString;
mod threaded;

pub use threaded::run_threaded;

/// Why a run stopped early.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FaultKind {
    Overflow,
    DivisionByZero,
    ShiftAmount,
    Conversion,
    Budget,
    Depth,
    OutOfMemory,
    /// Raised by a host function, with its message.
    Host(String),
}

impl From<eitri_bytecode::scalar::ScalarFault> for FaultKind {
    fn from(value: eitri_bytecode::scalar::ScalarFault) -> Self {
        use eitri_bytecode::scalar::ScalarFault;
        match value {
            ScalarFault::Overflow => Self::Overflow,
            ScalarFault::DivisionByZero => Self::DivisionByZero,
            ScalarFault::ShiftAmount => Self::ShiftAmount,
            ScalarFault::Conversion => Self::Conversion,
        }
    }
}

impl FaultKind {
    /// The message the first slice specifies.
    pub fn message(&self) -> &str {
        match self {
            FaultKind::Overflow => "integer overflow",
            FaultKind::DivisionByZero => "division by zero",
            FaultKind::ShiftAmount => "invalid shift amount",
            FaultKind::Conversion => "numeric conversion out of range",
            FaultKind::Budget => "execution budget exhausted",
            FaultKind::Depth => "call depth exceeded",
            FaultKind::OutOfMemory => "managed memory limit exceeded",
            FaultKind::Host(message) => message,
        }
    }
}

/// One trace entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Location {
    /// A script function at the instruction that faulted, or at a call
    /// still active in an outer frame.
    Script { function: FunctionId, pc: u32 },
    /// A host function: one that raised the fault, or that reentered and
    /// propagated a fault from the nested run.
    Host(u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fault {
    pub kind: FaultKind,
    /// Innermost first.
    pub trace: Vec<Location>,
}

/// The host functions a program calls, by index.
pub trait Host {
    /// Call host function `index`. Arguments and results are reached
    /// through `cx`, which also reenters the runtime. An error is a fault
    /// at the call.
    fn call(&mut self, cx: &mut Call<'_, '_>, index: u32) -> Result<(), HostError>;
}

/// Why a host function failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostError {
    /// A fault with this message, raised by the host function.
    Message(String),
    /// A fault from a nested run, propagated: the run stops with it, its
    /// trace continued through the host function.
    Fault(Fault),
}

impl From<FaultKind> for HostError {
    fn from(kind: FaultKind) -> Self {
        HostError::Fault(Fault {
            kind,
            trace: Vec::new(),
        })
    }
}

impl From<String> for HostError {
    fn from(message: String) -> Self {
        HostError::Message(message)
    }
}

impl From<&str> for HostError {
    fn from(message: &str) -> Self {
        HostError::Message(message.to_owned())
    }
}

impl From<Fault> for HostError {
    fn from(fault: Fault) -> Self {
        HostError::Fault(fault)
    }
}

/// A script function a host may reenter, from [`Call::function`], valid
/// only during the host call that found it: `'call` is that call, and
/// [`Host::call`] receives a fresh one each time, so a host cannot keep a
/// handle past its call, where the program could be dropped or replaced
/// and the handle could name another program's function.
///
/// Looking it up again on each call is a search by name over the
/// program's functions. Keeping it does not compile:
///
/// ```compile_fail
/// use eitri_vm::{Call, Host, HostError, ScriptFn};
/// struct Caching<'a> {
///     callback: Option<ScriptFn<'a>>,
/// }
/// impl<'a> Host for Caching<'a> {
///     fn call(&mut self, cx: &mut Call<'_, '_>, _: u32) -> Result<(), HostError> {
///         self.callback = cx.function("callback");
///         Ok(())
///     }
/// }
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScriptFn<'call> {
    id: FunctionId,
    call: PhantomData<&'call ()>,
}

/// An argument to a host-initiated script call.
pub enum ReentryArg<'a> {
    /// A canonical untagged scalar matching the declared parameter.
    Scalar(u64),
    /// A UTF-8 value copied into the managed heap for the call.
    String(&'a str),
    /// A context-owned retained string, passed without copying.
    RetainedString(&'a RetainedString),
}

/// The result of a host-initiated script call. String results are copied out
/// so they remain valid after further reentry or return from the host call.
#[derive(Debug, PartialEq, Eq)]
pub enum ReentryValue {
    Void,
    Scalar(u64),
    String(String),
}

/// Nested runs active at once. Each costs Rust stack (a host frame and an
/// interpreter entry), so their nesting is limited separately from script
/// frames; exceeding it is `call depth exceeded`, like the frame limit.
/// Measured per nested run, excluding the host's own frame: 688 B
/// (`match` loop) and 896 B (threaded) in release, 14.9 KB and 3.5 KB in
/// debug builds, so 64 fit a default 2 MiB thread in every build.
pub const MAX_NESTING: u32 = 64;

/// The state a run shares with the runs its host functions start.
struct Machine<'a> {
    functions: &'a IndexVec<FunctionId, Function>,
    slots: &'a mut Vec<u64>,
    frames: &'a mut Vec<Frame>,
    heap: &'a mut heap::Heap,
    references: &'a mut Vec<Option<heap::HeapRef>>,
    host_roots: &'a mut Vec<heap::HeapRef>,
    host_signatures: &'a [eitri_bytecode::Signature],
    memory: &'a mut Option<eitri_base::MemoryBudget>,
    force_gc: bool,
    memory_limit: usize,
    budget: u64,
    depth: u32,
    /// The run's entry function, which a host may not reenter.
    entry: FunctionId,
    /// Nested runs active.
    nesting: u32,
    threaded: bool,
}

/// A host function's view of its call: the argument and result windows in
/// the caller's frame, and reentry. Reentering takes `&mut self`, so no
/// argument or result borrow can be held across it: a nested run may grow
/// and move the slot stack.
///
/// Reading arguments before reentering, and results after, compiles:
///
/// ```
/// use eitri_vm::{Call, Host, HostError};
/// struct Twice;
/// impl Host for Twice {
///     fn call(&mut self, cx: &mut Call<'_, '_>, _: u32) -> Result<(), HostError> {
///         let arg = cx.arg(0);
///         let f = cx.function("callback").ok_or("no callback")?;
///         let mut result = [0];
///         cx.call(self, f, &[arg], &mut result)?;
///         cx.set_result(0, result[0] * 2);
///         Ok(())
///     }
/// }
/// ```
///
/// Holding the argument view across reentry does not:
///
/// ```compile_fail,E0502
/// use eitri_vm::{Call, Host, HostError};
/// struct Stale;
/// impl Host for Stale {
///     fn call(&mut self, cx: &mut Call<'_, '_>, _: u32) -> Result<(), HostError> {
///         let args = cx.args();
///         let f = cx.function("callback").ok_or("no callback")?;
///         cx.call(self, f, &[], &mut [])?;
///         let _stale = args[0];
///         Ok(())
///     }
/// }
/// ```
pub struct Call<'c, 'a> {
    machine: &'c mut Machine<'a>,
    /// The calling script frame, pushed below a nested run for its return
    /// and for traces.
    caller: Frame,
    /// The first slot after the caller's frame: where a nested run starts.
    top: usize,
    /// Absolute slot ranges.
    args: SlotRange,
    results: SlotRange,
    host_index: u32,
}

impl<'c> Call<'c, '_> {
    #[inline]
    pub fn args(&self) -> &[u64] {
        &self.machine.slots[self.args.slots_usize()]
    }

    #[inline]
    pub fn arg(&self, index: usize) -> u64 {
        self.args()[index]
    }

    #[inline]
    pub fn results(&mut self) -> &mut [u64] {
        &mut self.machine.slots[self.results.slots_usize()]
    }

    #[inline]
    pub fn set_result(&mut self, index: usize, value: u64) {
        self.results()[index] = value;
    }

    /// Borrow a UTF-8 argument until the next mutable use of this call.
    /// The view cannot be retained across reentry:
    ///
    /// ```compile_fail,E0502
    /// use eitri_vm::{Call, Host, HostError};
    /// struct Callback;
    /// impl Host for Callback {
    ///     fn call(&mut self, cx: &mut Call<'_, '_>, _: u32) -> Result<(), HostError> {
    ///         let text = cx.string_arg(0).unwrap();
    ///         let f = cx.function("callback").unwrap();
    ///         cx.call(self, f, &[], &mut [])?;
    ///         println!("{text}");
    ///         Ok(())
    ///     }
    /// }
    /// ```
    pub fn string_arg(&self, index: usize) -> Option<&str> {
        let signature = self.machine.host_signatures.get(self.host_index as usize)?;
        if signature.params.get(index) != Some(&eitri_bytecode::ValueType::String) {
            return None;
        }
        let reference = self
            .machine
            .references
            .get(self.args.first as usize + index)?
            .as_ref()?;
        self.machine.heap.string(*reference)
    }

    /// Copy a UTF-8 string into a declared string result slot.
    pub fn set_result_string(&mut self, index: usize, value: &str) -> Result<(), HostError> {
        let m = &mut *self.machine;
        if m.host_signatures
            .get(self.host_index as usize)
            .and_then(|s| s.results.get(index))
            != Some(&eitri_bytecode::ValueType::String)
        {
            return Err(HostError::from(
                "host string result does not match its signature",
            ));
        }
        if m.force_gc {
            collect_at(
                m.heap,
                m.references,
                (m.host_roots, m.frames, m.memory),
                m.functions,
                self.caller.function,
                self.caller.base,
                self.caller.call_pc as usize,
            )?;
        }
        let mut result = m.heap.alloc_string(value, m.memory_limit, m.memory);
        if result.is_err() {
            collect_at(
                m.heap,
                m.references,
                (m.host_roots, m.frames, m.memory),
                m.functions,
                self.caller.function,
                self.caller.base,
                self.caller.call_pc as usize,
            )?;
            result = m.heap.alloc_string(value, m.memory_limit, m.memory);
        }
        let reference = result.map_err(|_| {
            HostError::Fault(Fault {
                kind: FaultKind::OutOfMemory,
                trace: Vec::new(),
            })
        })?;
        push_vm(m.host_roots, reference, m.memory)?;
        let length = m.references.len().max(self.results.end() as usize);
        resize_vm(m.references, length, None, m.memory)?;
        m.references[self.results.first as usize + index] = Some(reference);
        Ok(())
    }

    /// Keep a string argument alive across this call and future executions.
    /// Release the returned token through this context when finished.
    pub fn retain_string_arg(&mut self, index: usize) -> Result<Option<RetainedString>, HostError> {
        let m = &mut *self.machine;
        if m.host_signatures
            .get(self.host_index as usize)
            .and_then(|s| s.params.get(index))
            != Some(&eitri_bytecode::ValueType::String)
        {
            return Ok(None);
        }
        let Some(reference) = m
            .references
            .get(self.args.first as usize + index)
            .copied()
            .flatten()
        else {
            return Ok(None);
        };
        let retained = m
            .heap
            .retain_string(reference, m.memory)
            .map_err(|_| HostError::from(FaultKind::OutOfMemory))?;
        Ok(Some(retained))
    }

    /// Return a previously retained string without copying its contents.
    pub fn set_result_retained_string(
        &mut self,
        index: usize,
        handle: &RetainedString,
    ) -> Result<(), HostError> {
        let m = &mut *self.machine;
        if m.host_signatures
            .get(self.host_index as usize)
            .and_then(|s| s.results.get(index))
            != Some(&eitri_bytecode::ValueType::String)
        {
            return Err(HostError::from(
                "host string result does not match its signature",
            ));
        }
        let reference = m
            .heap
            .retained_string(handle)
            .ok_or("invalid retained string")?;
        // Keep the result alive even if the host releases the token before return.
        push_vm(m.host_roots, reference, m.memory)?;
        let length = m.references.len().max(self.results.end() as usize);
        resize_vm(m.references, length, None, m.memory)?;
        m.references[self.results.first as usize + index] = Some(reference);
        Ok(())
    }

    /// Release a token during a host call.
    pub fn release_retained_string(&mut self, handle: &RetainedString) -> bool {
        self.machine.heap.release_string(handle)
    }

    fn finish_host(&self) -> Result<(), HostError> {
        if let Some(signature) = self.machine.host_signatures.get(self.host_index as usize) {
            for (i, ty) in signature.results.iter().enumerate() {
                if *ty == eitri_bytecode::ValueType::String
                    && self
                        .machine
                        .references
                        .get(self.results.first as usize + i)
                        .and_then(|v| *v)
                        .is_none()
                {
                    return Err(HostError::from("host did not initialize a string result"));
                }
            }
        }
        Ok(())
    }

    fn prepare_host(&mut self) -> Result<(), HostError> {
        if self
            .machine
            .host_signatures
            .get(self.host_index as usize)
            .is_some_and(|s| s.results.contains(&eitri_bytecode::ValueType::String))
        {
            let length = self
                .machine
                .references
                .len()
                .max(self.results.end() as usize);
            resize_vm(self.machine.references, length, None, self.machine.memory)?;
            self.machine.references[self.results.slots_usize()].fill(None);
        }
        Ok(())
    }

    /// The script function named `name`, if the program has one, for this
    /// call ([`ScriptFn`]). A search by name over the program's functions.
    pub fn function(&self, name: &str) -> Option<ScriptFn<'c>> {
        let m = &*self.machine;
        m.functions
            .iter_enumerated()
            .find(|&(id, f)| id != m.entry && &*f.name == name)
            .map(|(id, _)| ScriptFn {
                id,
                call: PhantomData,
            })
    }

    /// Run script function `function` with `args`, writing `results`. The
    /// call debits one step and counts as a frame, as a script call does;
    /// host calls inside it go to `host`, normally the caller's `self`. A
    /// fault stops only the nested run: the host may handle it or return it
    /// (`?`) to stop the outer run too. Its trace ends at the nested entry.
    pub fn call(
        &mut self,
        host: &mut dyn Host,
        function: ScriptFn<'_>,
        args: &[u64],
        results: &mut [u64],
    ) -> Result<(), Fault> {
        let m = &mut *self.machine;
        let callee = &m.functions[function.id];
        let signature = &callee.signature;
        let mismatch = args.len() != signature.params.len()
            || results.len() != signature.results.len()
            || signature
                .results
                .contains(&eitri_bytecode::ValueType::String)
            || args
                .iter()
                .zip(&signature.params)
                .any(|(&arg, &ty)| !ty.canonical(arg));
        if mismatch {
            let message = format!(
                "host error: reentrant call to `{}` does not match its signature",
                callee.name
            );
            return Err(Fault {
                kind: FaultKind::Host(message),
                trace: Vec::new(),
            });
        }
        let stop = |kind| {
            Err(Fault {
                kind,
                trace: Vec::new(),
            })
        };
        if m.budget == 0 {
            return stop(FaultKind::Budget);
        }
        m.budget -= 1;
        if m.frames.len() as u32 >= m.depth || m.nesting >= MAX_NESTING {
            return stop(FaultKind::Depth);
        }
        let base = self.top;
        let end = base + callee.frame_size as usize;
        if m.slots.len() < end {
            resize_vm(m.slots, end, 0, m.memory).map_err(|kind| Fault {
                kind,
                trace: Vec::new(),
            })?;
        }
        let first = base + signature.param_base() as usize;
        m.slots[first..first + args.len()].copy_from_slice(args);
        push_vm(m.frames, self.caller, m.memory).map_err(|kind| Fault {
            kind,
            trace: Vec::new(),
        })?;
        let floor = m.frames.len();
        m.nesting += 1;
        let outcome = if m.threaded {
            threaded::nested(m, host, function.id, base, floor)
        } else {
            execute(m, host, function.id, base, floor)
        };
        m.nesting -= 1;
        m.frames.truncate(floor - 1);
        outcome?;
        results.copy_from_slice(&m.slots[base..base + results.len()]);
        Ok(())
    }
    /// Call a script function with typed string and scalar arguments. String
    /// inputs are rooted during preparation and execution; string results are
    /// copied before the nested frame is released. Uses the active call budget.
    pub fn call_values(
        &mut self,
        host: &mut dyn Host,
        function: ScriptFn<'_>,
        args: &[ReentryArg<'_>],
    ) -> Result<ReentryValue, Fault> {
        use eitri_bytecode::ValueType;
        let m = &mut *self.machine;
        let callee = &m.functions[function.id];
        let signature = &callee.signature;
        let invalid = args.len() != signature.params.len()
            || signature.results.len() > 1
            || args
                .iter()
                .zip(&signature.params)
                .any(|(arg, ty)| match arg {
                    ReentryArg::Scalar(word) => !ty.canonical(*word),
                    ReentryArg::String(_) => *ty != ValueType::String,
                    ReentryArg::RetainedString(handle) => {
                        *ty != ValueType::String || m.heap.retained_string(handle).is_none()
                    }
                });
        if invalid {
            return Err(Fault {
                kind: FaultKind::Host(format!(
                    "host error: reentrant call to `{}` does not match its signature",
                    callee.name
                )),
                trace: Vec::new(),
            });
        }
        let fail = |kind| Fault {
            kind,
            trace: Vec::new(),
        };
        if m.budget == 0 {
            return Err(fail(FaultKind::Budget));
        }
        m.budget -= 1;
        if m.frames.len() as u32 >= m.depth || m.nesting >= MAX_NESTING {
            return Err(fail(FaultKind::Depth));
        }
        let base = self.top;
        let end = base + callee.frame_size as usize;
        if m.slots.len() < end {
            resize_vm(m.slots, end, 0, m.memory).map_err(fail)?;
        }
        if signature.params.contains(&ValueType::String)
            || signature.results.contains(&ValueType::String)
        {
            let length = m.references.len().max(end);
            resize_vm(m.references, length, None, m.memory).map_err(fail)?;
        }
        let first = base + signature.param_base() as usize;
        let root_floor = m.host_roots.len();
        let outcome = (|| {
            for (i, arg) in args.iter().enumerate() {
                match arg {
                    ReentryArg::Scalar(word) => m.slots[first + i] = *word,
                    ReentryArg::RetainedString(handle) => {
                        let value = m.heap.retained_string(handle).expect("validated handle");
                        m.references[first + i] = Some(value);
                        push_vm(m.host_roots, value, m.memory).map_err(fail)?;
                    }
                    ReentryArg::String(text) => {
                        if m.force_gc {
                            collect_at(
                                m.heap,
                                m.references,
                                (m.host_roots, m.frames, m.memory),
                                m.functions,
                                self.caller.function,
                                self.caller.base,
                                self.caller.call_pc as usize,
                            )
                            .map_err(fail)?;
                        }
                        let mut value = m.heap.alloc_string(text, m.memory_limit, m.memory);
                        if value.is_err() {
                            collect_at(
                                m.heap,
                                m.references,
                                (m.host_roots, m.frames, m.memory),
                                m.functions,
                                self.caller.function,
                                self.caller.base,
                                self.caller.call_pc as usize,
                            )
                            .map_err(fail)?;
                            value = m.heap.alloc_string(text, m.memory_limit, m.memory);
                        }
                        let value = value.map_err(|_| fail(FaultKind::OutOfMemory))?;
                        m.references[first + i] = Some(value);
                        push_vm(m.host_roots, value, m.memory).map_err(fail)?;
                    }
                }
            }
            push_vm(m.frames, self.caller, m.memory).map_err(fail)?;
            let floor = m.frames.len();
            m.nesting += 1;
            let result = if m.threaded {
                threaded::nested(m, host, function.id, base, floor)
            } else {
                execute(m, host, function.id, base, floor)
            };
            m.nesting -= 1;
            m.frames.truncate(floor - 1);
            result?;
            Ok(match signature.results.first() {
                None => ReentryValue::Void,
                Some(ValueType::String) => {
                    let reference = m.references[base].expect("verified string result");
                    let text = m.heap.string(reference).expect("live string result");
                    let mut result = String::new();
                    result
                        .try_reserve_exact(text.len())
                        .map_err(|_| fail(FaultKind::OutOfMemory))?;
                    result.push_str(text);
                    ReentryValue::String(result)
                }
                Some(_) => ReentryValue::Scalar(m.slots[base]),
            })
        })();
        m.host_roots.truncate(root_floor);
        outcome
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Steps: each call and each taken backward branch costs one.
    pub budget: u64,
    /// Active script frames below the entry function.
    pub depth: u32,
}

impl Default for Limits {
    /// The first slice's defaults.
    fn default() -> Self {
        Limits {
            budget: 1_000_000,
            depth: 4096,
        }
    }
}

/// A suspended caller.
#[derive(Clone, Copy)]
struct Frame {
    function: FunctionId,
    base: usize,
    /// The caller's `CALL`, for traces.
    call_pc: u32,
    /// Where the caller resumes.
    resume: usize,
    /// The caller's slot that receives the callee's result span.
    result: u32,
}

/// The interpreter's reusable working memory (D44).
pub struct VmScratch {
    slots: Vec<u64>,
    frames: Vec<Frame>,
    // References are carried separately from scalar words so their pointer
    // provenance is preserved even on machines where pointers are not u64s.
    references: Vec<Option<heap::HeapRef>>,
    host_roots: Vec<heap::HeapRef>,
    host_signatures: Vec<eitri_bytecode::Signature>,
    memory: Option<eitri_base::MemoryBudget>,
    heap: heap::Heap,
    force_gc: bool,
    memory_limit: usize,
}

impl Default for VmScratch {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            frames: Vec::new(),
            references: Vec::new(),
            host_roots: Vec::new(),
            host_signatures: Vec::new(),
            memory: None,
            heap: heap::Heap::default(),
            force_gc: false,
            memory_limit: usize::MAX,
        }
    }
}

impl VmScratch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Release a retained string issued by this context. Returns false for
    /// a stale token or one belonging to another context.
    pub fn release_retained_string(&mut self, handle: &RetainedString) -> bool {
        self.heap.release_string(handle)
    }

    /// Install the registered host signatures used to validate string access.
    /// Call this before running bytecode that calls a host function with a
    /// string parameter or result. `eitri::Context` installs them on registration.
    pub fn set_host_signatures(&mut self, signatures: Vec<eitri_bytecode::Signature>) {
        self.host_signatures = signatures;
    }

    /// Install the operation's explicit full-runtime budget. Existing
    /// retained VM storage and the active artifact must already be charged.
    pub fn set_runtime_budget(&mut self, budget: Option<eitri_base::MemoryBudget>) {
        self.memory = budget;
    }

    /// Collect at each execution safepoint when managed execution is enabled.
    pub fn set_force_gc(&mut self, enabled: bool) {
        self.force_gc = enabled;
    }

    /// Limit accounted managed object bytes. Other runtime allocations are not
    /// yet covered by this limit.
    pub fn set_heap_limit(&mut self, bytes: usize) {
        self.memory_limit = bytes;
    }

    pub fn managed_heap_bytes(&self) -> usize {
        self.heap.bytes()
    }

    /// Retained VM storage with a conservative heap-index charge.
    pub fn budget_bytes(&self) -> Result<usize, eitri_base::OutOfMemory> {
        self.heap_bytes()
            .checked_sub(self.heap.bytes() + self.heap.retained_metadata_bytes())
            .and_then(|n| n.checked_add(self.heap.budget_bytes().ok()?))
            .ok_or(eitri_base::OutOfMemory)
    }

    /// Heap bytes retained, for scratch high-water accounting.
    pub fn heap_bytes(&self) -> usize {
        self.slots.capacity() * 8
            + self.frames.capacity() * size_of::<Frame>()
            + self.references.capacity() * size_of::<Option<heap::HeapRef>>()
            + self.host_roots.capacity() * size_of::<heap::HeapRef>()
            + self.host_signatures.capacity() * size_of::<eitri_bytecode::Signature>()
            + self
                .host_signatures
                .iter()
                .map(|s| {
                    (s.params.capacity() + s.results.capacity())
                        * size_of::<eitri_bytecode::ValueType>()
                })
                .sum::<usize>()
            + self.heap.bytes()
            + self.heap.retained_metadata_bytes()
    }
}

/// Grow VM-owned vector storage fallibly, charging reallocation peak before
/// growth when a full runtime budget is active.
fn reserve_vm<T>(
    values: &mut Vec<T>,
    additional: usize,
    memory: &mut Option<eitri_base::MemoryBudget>,
) -> Result<(), FaultKind> {
    if values
        .len()
        .checked_add(additional)
        .is_none_or(|n| n > values.capacity())
    {
        if let Some(budget) = memory {
            budget
                .reserve_vec(values, additional)
                .map_err(|_| FaultKind::OutOfMemory)?;
        } else {
            values
                .try_reserve(additional)
                .map_err(|_| FaultKind::OutOfMemory)?;
        }
    }
    Ok(())
}

fn reserve_string_vm(
    value: &mut String,
    additional: usize,
    memory: &mut Option<eitri_base::MemoryBudget>,
) -> Result<(), FaultKind> {
    if let Some(budget) = memory {
        budget
            .reserve_string(value, additional)
            .map_err(|_| FaultKind::OutOfMemory)
    } else {
        value
            .try_reserve_exact(additional)
            .map_err(|_| FaultKind::OutOfMemory)
    }
}

fn charge_vm(bytes: usize, memory: &mut Option<eitri_base::MemoryBudget>) -> Result<(), FaultKind> {
    if let Some(budget) = memory {
        budget.charge(bytes).map_err(|_| FaultKind::OutOfMemory)?;
    }
    Ok(())
}

fn release_vm(bytes: usize, memory: &mut Option<eitri_base::MemoryBudget>) {
    if let Some(budget) = memory {
        budget.release(bytes);
    }
}

fn reserve_map_vm<K: Eq + std::hash::Hash, V, S: std::hash::BuildHasher>(
    values: &mut std::collections::HashMap<K, V, S>,
    additional: usize,
    memory: &mut Option<eitri_base::MemoryBudget>,
) -> Result<(), FaultKind> {
    if let Some(budget) = memory {
        budget
            .reserve_map(values, additional)
            .map_err(|_| FaultKind::OutOfMemory)
    } else {
        values
            .try_reserve(additional)
            .map_err(|_| FaultKind::OutOfMemory)
    }
}

fn resize_vm<T: Clone>(
    values: &mut Vec<T>,
    length: usize,
    value: T,
    memory: &mut Option<eitri_base::MemoryBudget>,
) -> Result<(), FaultKind> {
    let additional = length.saturating_sub(values.len());
    reserve_vm(values, additional, memory)?;
    values.resize(length, value);
    Ok(())
}

fn push_vm<T>(
    values: &mut Vec<T>,
    value: T,
    memory: &mut Option<eitri_base::MemoryBudget>,
) -> Result<(), FaultKind> {
    reserve_vm(values, 1, memory)?;
    values.push(value);
    Ok(())
}

/// Run `entry` with `args` and return its results. `functions` must be
/// verified.
pub fn run(
    functions: &IndexVec<FunctionId, Function>,
    entry: FunctionId,
    args: &[u64],
    host: &mut dyn Host,
    limits: Limits,
    scratch: &mut VmScratch,
) -> Result<Vec<u64>, Fault> {
    let mut m = machine(functions, entry, args, limits, scratch, false)?;
    let result = execute(&mut m, host, entry, 0, 0);
    m.heap.collect([], m.memory);
    result?;
    copy_result(&m.slots[..functions[entry].signature.results.len()])
}

fn copy_result(values: &[u64]) -> Result<Vec<u64>, Fault> {
    let mut result = Vec::new();
    result.try_reserve_exact(values.len()).map_err(|_| Fault {
        kind: FaultKind::OutOfMemory,
        trace: Vec::new(),
    })?;
    result.extend_from_slice(values);
    Ok(result)
}

/// The shared state of a fresh run, with `entry`'s frame at slot 0.
fn machine<'a>(
    functions: &'a IndexVec<FunctionId, Function>,
    entry: FunctionId,
    args: &[u64],
    limits: Limits,
    scratch: &'a mut VmScratch,
    threaded: bool,
) -> Result<Machine<'a>, Fault> {
    scratch.slots.clear();
    scratch.frames.clear();
    scratch.references.clear();
    scratch.host_roots.clear();
    let function = &functions[entry];
    resize_vm(
        &mut scratch.slots,
        function.frame_size as usize,
        0,
        &mut scratch.memory,
    )
    .map_err(|kind| Fault {
        kind,
        // Do not allocate a trace while reporting failure to allocate the
        // entry frame. There is no executing instruction yet.
        trace: Vec::new(),
    })?;
    let first = function.signature.param_base() as usize;
    scratch.slots[first..first + args.len()].copy_from_slice(args);
    Ok(Machine {
        functions,
        slots: &mut scratch.slots,
        frames: &mut scratch.frames,
        heap: &mut scratch.heap,
        references: &mut scratch.references,
        host_roots: &mut scratch.host_roots,
        host_signatures: &scratch.host_signatures,
        memory: &mut scratch.memory,
        force_gc: scratch.force_gc,
        memory_limit: scratch.memory_limit,
        budget: limits.budget,
        depth: limits.depth,
        entry,
        nesting: 0,
        threaded,
    })
}

/// Collect from the verifier-approved maps for suspended and active frames.
/// Reference slots are kept apart from scalar words to preserve provenance.
fn collect_at(
    heap: &mut heap::Heap,
    references: &mut Vec<Option<heap::HeapRef>>,
    (host_roots, frames, memory): (
        &[heap::HeapRef],
        &[Frame],
        &mut Option<eitri_base::MemoryBudget>,
    ),
    functions: &IndexVec<FunctionId, Function>,
    id: FunctionId,
    base: usize,
    pc: usize,
) -> Result<(), FaultKind> {
    let function = &functions[id];
    let length = references.len().max(base + function.frame_size as usize);
    resize_vm(references, length, None, memory)?;
    fn root_map(function: &Function, pc: u32, suspended: bool) -> &eitri_bytecode::RootMap {
        let insn =
            eitri_bytecode::decode(&function.code, pc as usize).expect("verified instruction");
        let state = if matches!(insn.op, Op::CallScript | Op::CallHost) {
            let roots = &function.roots.calls[insn.operands.b as usize];
            if suspended {
                &roots.suspended
            } else {
                &roots.pre
            }
        } else {
            let index = function
                .roots
                .safepoints
                .binary_search_by_key(&eitri_bytecode::SafepointPc(pc), |(pc, _)| *pc)
                .expect("verified safepoint");
            &function.roots.safepoints[index].1
        };
        &function.roots.maps[state.map.0 as usize]
    }
    let roots = frames
        .iter()
        .map(|frame| {
            let f = &functions[frame.function];
            heap::RootFrame {
                map: root_map(f, frame.call_pc, true),
                slots: &references[frame.base..frame.base + f.frame_size as usize],
            }
        })
        .chain(std::iter::once(heap::RootFrame {
            map: root_map(function, pc as u32, false),
            slots: &references[base..base + function.frame_size as usize],
        }));
    heap.collect_frames(roots, host_roots, memory)
        .expect("verified supported root maps");
    Ok(())
}

/// `range`, a window of the frame at `base`, as absolute slots.
#[inline(always)]
fn absolute(range: SlotRange, base: usize) -> SlotRange {
    SlotRange {
        first: base as u32 + range.first,
        count: range.count,
    }
}

/// Field extraction for short forms; `hi` is a `WIDE` prefix word or 0.
#[inline(always)]
fn slot_field(word: u32, hi: u32, shift: u32) -> usize {
    (((word >> shift) & 0xff) | ((hi >> shift) & 0xff) << 8) as usize
}

/// Copy `count` slots from `from` to `to`. Calls usually move one slot,
/// which is copied inline instead of through a call to `memmove` (E4, V4).
#[inline(always)]
fn copy_slots(slots: &mut [u64], from: usize, to: usize, count: usize) {
    if count == 1 {
        slots[to] = slots[from];
    } else {
        slots.copy_within(from..from + count, to);
    }
}

/// A fault at `pc` in `function`, with the active calls of this run
/// (`frames`, above its floor) below it.
#[cold]
fn fault(frames: &[Frame], kind: FaultKind, function: FunctionId, pc: usize) -> Fault {
    raised(Vec::new(), frames, kind, function, pc)
}

/// A fault whose innermost entries are `inner` (a host function, or a
/// nested run and the host function that propagated it), raised at `pc`.
#[cold]
fn raised(
    mut trace: Vec<Location>,
    frames: &[Frame],
    kind: FaultKind,
    function: FunctionId,
    pc: usize,
) -> Fault {
    if trace.try_reserve(frames.len().saturating_add(1)).is_err() {
        return Fault {
            kind: FaultKind::OutOfMemory,
            trace: Vec::new(),
        };
    }
    trace.push(Location::Script {
        function,
        pc: pc as u32,
    });
    trace.extend(frames.iter().rev().map(|frame| Location::Script {
        function: frame.function,
        pc: frame.call_pc,
    }));
    Fault { kind, trace }
}

/// The fault a failed host function `index` raises at its call.
#[cold]
fn host_fault(
    error: HostError,
    index: u32,
    frames: &[Frame],
    function: FunctionId,
    pc: usize,
) -> Fault {
    let (kind, mut inner) = match error {
        HostError::Message(message) => (FaultKind::Host(message), Vec::new()),
        HostError::Fault(fault) => (fault.kind, fault.trace),
    };
    if inner.try_reserve(1).is_err() {
        return Fault {
            kind: FaultKind::OutOfMemory,
            trace: Vec::new(),
        };
    }
    inner.push(Location::Host(index));
    raised(inner, frames, kind, function, pc)
}

/// The interpreter loop. The current function's code and frame are held
/// as local slices and re-derived only by `CALL` and `RET`, so the
/// instruction head does not reload them (E5, V2).
///
/// Runs `entry`, whose frame is in place at `base`, until it returns (its
/// results are then at `base`) or faults. Frames below `floor` belong to
/// outer runs: a nested run returns to its host when its entry returns,
/// and its faults trace only its own frames.
fn execute(
    m: &mut Machine<'_>,
    host: &mut dyn Host,
    entry: FunctionId,
    base: usize,
    floor: usize,
) -> Result<(), Fault> {
    let functions = m.functions;
    let depth = m.depth;
    let mut budget = m.budget;
    let mut id = entry;
    let mut function = &functions[id];
    let mut base = base;
    let mut code: &[u32] = &function.code;
    let mut s: &mut [u64] = &mut m.slots[base..base + function.frame_size as usize];
    let mut pc = 0usize;

    loop {
        let start = pc;
        let word = code[pc];
        pc += 1;

        macro_rules! checked {
            ($slot:expr, $result:expr, $kind:expr) => {
                match $result {
                    Some(value) => s[$slot] = value as u64,
                    None => return Err(fault(&m.frames[floor..], $kind, id, start)),
                }
            };
        }
        macro_rules! poll {
            () => {
                if budget == 0 {
                    return Err(fault(&m.frames[floor..], FaultKind::Budget, id, start));
                }
                budget -= 1;
            };
        }
        macro_rules! branch {
            ($taken:expr, $disp:expr) => {
                if $taken {
                    let target = (pc as isize + $disp as isize) as usize;
                    // A taken backward branch polls the budget.
                    if target <= start {
                        poll!();
                        if m.force_gc {
                            collect_at(
                                m.heap,
                                m.references,
                                (m.host_roots, m.frames, m.memory),
                                functions,
                                id,
                                base,
                                start,
                            )
                            .map_err(|kind| fault(&m.frames[floor..], kind, id, start))?;
                        }
                    }
                    pc = target;
                }
            };
        }
        let int = |slot: usize, s: &[u64]| s[slot] as i64;

        // The handlers, expanded twice: with `hi` a constant 0 on the
        // common path, so operand decoding drops the `WIDE` merge, and with
        // the prefix word after `WIDE` (E5, V5).
        macro_rules! dispatch {
            ($word:expr, $hi:expr) => {{
                let word: u32 = $word;
                let hi: u32 = $hi;
                let op = Op::from_byte(word as u8).expect("verified code");
                macro_rules! a {
                    () => {
                        slot_field(word, hi, 8)
                    };
                }
                macro_rules! b {
                    () => {
                        slot_field(word, hi, 16)
                    };
                }
                macro_rules! c {
                    () => {
                        slot_field(word, hi, 24)
                    };
                }
                // Immediates are never widened: the `Imm8` of `ALUI` (third
                // field) and of `CBI` (second field).
                macro_rules! imm {
                    () => {
                        (word >> 24) as i8 as i64
                    };
                }
                macro_rules! cbi_imm {
                    () => {
                        (word >> 16) as u8 as i8 as i64
                    };
                }
                match op {
                    Op::Mov => {
                        s[a!()] = s[b!()];
                        if !m.references.is_empty() {
                            let length =
                                m.references.len().max(base + function.frame_size as usize);
                            resize_vm(m.references, length, None, m.memory)
                                .map_err(|kind| fault(&m.frames[floor..], kind, id, start))?;
                            m.references[base + a!()] = m.references[base + b!()];
                        }
                    }
                    Op::StrNew => {
                        if m.force_gc {
                            collect_at(
                                m.heap,
                                m.references,
                                (m.host_roots, m.frames, m.memory),
                                functions,
                                id,
                                base,
                                start,
                            )
                            .map_err(|kind| fault(&m.frames[floor..], kind, id, start))?;
                        }
                        let length = m.references.len().max(base + function.frame_size as usize);
                        resize_vm(m.references, length, None, m.memory)
                            .map_err(|kind| fault(&m.frames[floor..], kind, id, start))?;
                        let literal = &function.strings[(word >> 16) as usize];
                        let mut allocated = m.heap.alloc_string(literal, m.memory_limit, m.memory);
                        if allocated.is_err() {
                            collect_at(
                                m.heap,
                                m.references,
                                (m.host_roots, m.frames, m.memory),
                                functions,
                                id,
                                base,
                                start,
                            )
                            .map_err(|kind| fault(&m.frames[floor..], kind, id, start))?;
                            allocated = m.heap.alloc_string(literal, m.memory_limit, m.memory);
                        }
                        m.references[base + a!()] = Some(allocated.map_err(|_| {
                            fault(&m.frames[floor..], FaultKind::OutOfMemory, id, start)
                        })?);
                    }
                    Op::StrConcat => {
                        if m.force_gc {
                            collect_at(
                                m.heap,
                                m.references,
                                (m.host_roots, m.frames, m.memory),
                                functions,
                                id,
                                base,
                                start,
                            )
                            .map_err(|kind| fault(&m.frames[floor..], kind, id, start))?;
                        }
                        let left = m.references[base + b!()].expect("verified string operand");
                        let right = m.references[base + c!()].expect("verified string operand");
                        let mut value = m.heap.concat(left, right, m.memory_limit, m.memory);
                        if value.is_err() {
                            collect_at(
                                m.heap,
                                m.references,
                                (m.host_roots, m.frames, m.memory),
                                functions,
                                id,
                                base,
                                start,
                            )
                            .map_err(|kind| fault(&m.frames[floor..], kind, id, start))?;
                            value = m.heap.concat(left, right, m.memory_limit, m.memory);
                        }
                        m.references[base + a!()] = Some(value.map_err(|_| {
                            fault(&m.frames[floor..], FaultKind::OutOfMemory, id, start)
                        })?);
                    }
                    Op::StrLen => {
                        let reference = m.references[base + b!()].expect("verified managed slot");
                        s[a!()] = m.heap.string(reference).expect("live string root").len() as u64;
                    }
                    Op::Ldi => s[a!()] = (word >> 16) as i16 as i64 as u64,
                    Op::Ldk => s[a!()] = function.consts[(word >> 16) as usize],
                    Op::LdkX => {
                        s[a!()] = function.consts[code[pc] as usize];
                        pc += 1;
                    }
                    Op::AddWrap => s[a!()] = int(b!(), s).wrapping_add(int(c!(), s)) as u64,
                    Op::SubWrap => s[a!()] = int(b!(), s).wrapping_sub(int(c!(), s)) as u64,
                    Op::MulWrap => s[a!()] = int(b!(), s).wrapping_mul(int(c!(), s)) as u64,
                    Op::AddChkS => checked!(
                        a!(),
                        int(b!(), s).checked_add(int(c!(), s)),
                        FaultKind::Overflow
                    ),
                    Op::SubChkS => checked!(
                        a!(),
                        int(b!(), s).checked_sub(int(c!(), s)),
                        FaultKind::Overflow
                    ),
                    Op::MulChkS => checked!(
                        a!(),
                        int(b!(), s).checked_mul(int(c!(), s)),
                        FaultKind::Overflow
                    ),
                    Op::DivS | Op::RemS => {
                        let (x, y) = (int(b!(), s), int(c!(), s));
                        if y == 0 {
                            return Err(fault(
                                &m.frames[floor..],
                                FaultKind::DivisionByZero,
                                id,
                                start,
                            ));
                        }
                        if op == Op::DivS {
                            checked!(a!(), x.checked_div(y), FaultKind::Overflow);
                        } else {
                            // `MIN % -1` is 0, which fits.
                            s[a!()] = x.wrapping_rem(y) as u64;
                        }
                    }
                    Op::Xor => s[a!()] = s[b!()] ^ s[c!()],
                    Op::AddiWrap => s[a!()] = int(b!(), s).wrapping_add(imm!()) as u64,
                    Op::SubiWrap => s[a!()] = int(b!(), s).wrapping_sub(imm!()) as u64,
                    Op::AddiChkS => {
                        checked!(a!(), int(b!(), s).checked_add(imm!()), FaultKind::Overflow)
                    }
                    Op::SubiChkS => {
                        checked!(a!(), int(b!(), s).checked_sub(imm!()), FaultKind::Overflow)
                    }
                    Op::MuliChkS => {
                        checked!(a!(), int(b!(), s).checked_mul(imm!()), FaultKind::Overflow)
                    }
                    Op::CbiEq => branch!(int(a!(), s) == cbi_imm!(), (word >> 24) as i8),
                    Op::CbiNe => branch!(int(a!(), s) != cbi_imm!(), (word >> 24) as i8),
                    Op::CbiLt => branch!(int(a!(), s) < cbi_imm!(), (word >> 24) as i8),
                    Op::CbiGt => branch!(int(a!(), s) > cbi_imm!(), (word >> 24) as i8),
                    Op::NegChkS => checked!(a!(), int(b!(), s).checked_neg(), FaultKind::Overflow),
                    Op::CbEq => branch!(s[a!()] == s[b!()], (word >> 24) as i8),
                    Op::CbNe => branch!(s[a!()] != s[b!()], (word >> 24) as i8),
                    Op::CbLt => branch!(int(a!(), s) < int(b!(), s), (word >> 24) as i8),
                    Op::CbLe => branch!(int(a!(), s) <= int(b!(), s), (word >> 24) as i8),
                    Op::TbTrue => branch!(s[a!()] != 0, (word >> 16) as i16),
                    Op::TbFalse => branch!(s[a!()] == 0, (word >> 16) as i16),
                    Op::CsetEq => s[a!()] = u64::from(s[b!()] == s[c!()]),
                    Op::CsetNe => s[a!()] = u64::from(s[b!()] != s[c!()]),
                    Op::CsetLt => s[a!()] = u64::from(int(b!(), s) < int(c!(), s)),
                    Op::CsetLe => s[a!()] = u64::from(int(b!(), s) <= int(c!(), s)),
                    Op::J => branch!(true, (word as i32) >> 8),
                    Op::JX => {
                        let disp = code[pc] as i32;
                        pc += 1;
                        branch!(true, disp);
                    }
                    Op::CallScript | Op::CallHost => {
                        let site = function.sites[(word >> 16) as usize];
                        poll!();
                        if m.force_gc {
                            collect_at(
                                m.heap,
                                m.references,
                                (m.host_roots, m.frames, m.memory),
                                functions,
                                id,
                                base,
                                start,
                            )
                            .map_err(|kind| fault(&m.frames[floor..], kind, id, start))?;
                        }
                        match site.target {
                            CallTarget::Host(index) => {
                                // Publish the budget: the host may reenter
                                // and spend from it.
                                m.budget = budget;
                                let frame_size = function.frame_size as usize;
                                let roots_floor = m.host_roots.len();
                                let mut cx = Call {
                                    machine: &mut *m,
                                    caller: Frame {
                                        function: id,
                                        base,
                                        call_pc: start as u32,
                                        resume: pc,
                                        result: 0,
                                    },
                                    top: base + frame_size,
                                    args: absolute(site.args, base),
                                    results: absolute(site.result, base),
                                    host_index: index,
                                };
                                let outcome = cx
                                    .prepare_host()
                                    .and_then(|()| host.call(&mut cx, index))
                                    .and_then(|()| cx.finish_host());
                                m.host_roots.truncate(roots_floor);
                                budget = m.budget;
                                // A nested run may have moved the slot stack.
                                s = &mut m.slots[base..base + frame_size];
                                if let Err(error) = outcome {
                                    return Err(host_fault(
                                        error,
                                        index,
                                        &m.frames[floor..],
                                        id,
                                        start,
                                    ));
                                }
                            }
                            CallTarget::Script(callee) => {
                                if m.frames.len() as u32 >= depth {
                                    return Err(fault(
                                        &m.frames[floor..],
                                        FaultKind::Depth,
                                        id,
                                        start,
                                    ));
                                }
                                let callee_fn = &functions[callee];
                                let callee_base = base + function.frame_size as usize;
                                let end = callee_base + callee_fn.frame_size as usize;
                                if m.slots.len() < end {
                                    resize_vm(m.slots, end, 0, m.memory).map_err(|kind| {
                                        fault(&m.frames[floor..], kind, id, start)
                                    })?;
                                }
                                let params =
                                    callee_base + callee_fn.signature.param_base() as usize;
                                let from = base + site.args.first as usize;
                                copy_slots(m.slots, from, params, site.args.count as usize);
                                if callee_fn
                                    .signature
                                    .params
                                    .contains(&eitri_bytecode::ValueType::String)
                                {
                                    let length = m.references.len().max(end);
                                    resize_vm(m.references, length, None, m.memory).map_err(
                                        |kind| fault(&m.frames[floor..], kind, id, start),
                                    )?;
                                    m.references
                                        .copy_within(from..from + site.args.count as usize, params);
                                }
                                push_vm(
                                    m.frames,
                                    Frame {
                                        function: id,
                                        base,
                                        call_pc: start as u32,
                                        resume: pc,
                                        result: site.result.first,
                                    },
                                    m.memory,
                                )
                                .map_err(|kind| fault(&m.frames[floor..], kind, id, start))?;
                                id = callee;
                                function = callee_fn;
                                base = callee_base;
                                code = &function.code;
                                s = &mut m.slots[base..end];
                                pc = 0;
                            }
                        }
                    }
                    Op::Ret => {
                        let count = function.signature.results.len();
                        if m.frames.len() == floor {
                            m.budget = budget;
                            return Ok(());
                        }
                        let caller = m.frames.pop().expect("above the floor");
                        let caller_fn = &functions[caller.function];
                        let to = caller.base + caller.result as usize;
                        copy_slots(m.slots, base, to, count);
                        if function
                            .signature
                            .results
                            .contains(&eitri_bytecode::ValueType::String)
                        {
                            m.references.copy_within(base..base + count, to);
                        }
                        id = caller.function;
                        function = caller_fn;
                        base = caller.base;
                        code = &function.code;
                        s = &mut m.slots[base..base + function.frame_size as usize];
                        pc = caller.resume;
                    }
                    other => {
                        let info = other.info();
                        if let eitri_bytecode::spec::Sem::Branch { cond, imm } = info.sem {
                            let rhs = if imm { cbi_imm!() } else { s[b!()] as i64 };
                            branch!(cond.eval(s[a!()] as i64, rhs), (word >> 24) as i8);
                        } else {
                            let x = if info.roles[1].is_slot() { s[b!()] } else { 0 };
                            let y = if info.roles[2].is_slot() { s[c!()] } else { 0 };
                            match eitri_bytecode::scalar::eval(info.sem, x, y, (word >> 24) as u8) {
                                Ok(value) => s[a!()] = value,
                                Err(kind) => {
                                    return Err(fault(&m.frames[floor..], kind.into(), id, start));
                                }
                            }
                        }
                    }
                }
            }};
        }
        // A single expansion keeps unoptimized reentry frames small enough
        // for the checked nesting limit. Release builds specialize both forms.
        #[cfg(debug_assertions)]
        {
            let (instruction, high) = if word as u8 == Op::Wide as u8 {
                let next = code[pc];
                pc += 1;
                (next, word)
            } else {
                (word, 0)
            };
            dispatch!(instruction, high);
        }
        #[cfg(not(debug_assertions))]
        if word as u8 == Op::Wide as u8 {
            let next = code[pc];
            pc += 1;
            dispatch!(next, word);
        } else {
            dispatch!(word, 0);
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod scalar_tests;
