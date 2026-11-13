//! Eitri implementation. This API is experimental and unstable.

mod context;

pub use context::{
    Compiled, Context, DEFAULT_TIER_THRESHOLD, FaultReport, OptimizeError, RunError, TraceFrame,
};
pub use eitri_bytecode::opt::Report as OptimizeReport;
pub use eitri_bytecode::text::PrintOptions;
/// The shared diagnostic model and renderer, re-exported so hosts need no
/// separate dependency.
pub use eitri_diag as diag;
pub use eitri_vm::{
    Call, Fault, FaultKind, Host, HostError, Limits, Location, RetainedString, ScriptFn,
};
