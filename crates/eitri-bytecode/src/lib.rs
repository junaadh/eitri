//! Eitri's typed bytecode: the machine-readable operation specification
//! (T003), instruction encoding and decoding, disassembly, and the
//! assembler and verifier that produce executable functions (T005).

mod asm;
mod disasm;
mod encoding;
mod format;
mod function;
pub mod ir;
pub mod opt;
pub mod oracle;
mod root_builder;
mod roots;
pub mod scalar;
pub mod spec;
pub mod text;
mod verify;

pub use asm::{AsmError, Assembler, Label, imm_complement};
pub use disasm::disassemble;
pub use encoding::{DecodeError, Decoded, EncodeError, MAX_SLOT, Operands, decode, encode};
pub use format::{FormatPart, FormatParts, format_parts};
pub use function::{CallSite, CallTarget, Function, FunctionId, Signature, SlotRange, ValueType};
pub use root_builder::{RootBuildError, RootBuilder, RootCheckpoint, RootLayout};
pub use roots::{
    CallRoots, FaultPc, FunctionRoots, GuardedRoot, RootFact, RootMap, RootMapId, RootShape,
    RootState, SafepointPc,
};
pub use spec::{AllocationClass, OPS, Op, OpInfo};
pub use verify::{
    Callees, ErrorKind, VerifyError, string_roots, string_roots_budgeted, verify, verify_budgeted,
    verify_root_state,
};

#[cfg(test)]
mod asm_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod opt_tests;

#[cfg(test)]
mod scalar_tests;
