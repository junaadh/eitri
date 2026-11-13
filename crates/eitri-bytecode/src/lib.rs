//! Eitri's typed bytecode: the machine-readable operation specification
//! (T003), instruction encoding and decoding, disassembly, and the
//! assembler and verifier that produce executable functions (T005).

mod asm;
mod disasm;
mod encoding;
mod function;
pub mod ir;
pub mod opt;
pub mod oracle;
mod roots;
pub mod scalar;
pub mod spec;
pub mod text;
mod verify;

pub use asm::{AsmError, Assembler, Label, imm_complement};
pub use disasm::disassemble;
pub use encoding::{DecodeError, Decoded, EncodeError, MAX_SLOT, Operands, decode, encode};
pub use function::{CallSite, CallTarget, Function, FunctionId, Signature, SlotRange, ValueType};
pub use roots::{CallRoots, FaultPc, FunctionRoots, RootMap, RootMapId, RootState, SafepointPc};
pub use spec::{AllocationClass, OPS, Op, OpInfo};
pub use verify::{Callees, ErrorKind, VerifyError, verify};

#[cfg(test)]
mod asm_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod opt_tests;

#[cfg(test)]
mod scalar_tests;
