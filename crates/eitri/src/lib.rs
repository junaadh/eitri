//! Eitri implementation. This API is experimental and unstable.

mod context;

pub use context::{Compiled, Context, FaultReport, RunError, TraceFrame};
/// The shared diagnostic model and renderer, re-exported so hosts need no
/// separate dependency.
pub use eitri_diag as diag;
pub use eitri_vm::{Call, Fault, FaultKind, Host, HostError, Limits, Location, ScriptFn};
